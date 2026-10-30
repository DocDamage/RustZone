//! WinMM MIDI input. Every open input device delivers its Control Change and Note On messages
//! as [`MidiEvent`]s on a channel; the UI decides what they control.

use crossbeam_channel::{Receiver, Sender};

pub type HMIDIIN = isize;
const CALLBACK_FUNCTION: u32 = 0x00030000;
const MIM_DATA: u32 = 0x3C3;

#[link(name = "winmm")]
extern "system" {
    fn midiInGetNumDevs() -> u32;
    fn midiInOpen(lphMidiIn: *mut HMIDIIN, uDeviceID: u32, dwCallback: usize, dwInstance: usize, dwFlags: u32) -> u32;
    fn midiInStart(hMidiIn: HMIDIIN) -> u32;
    fn midiInStop(hMidiIn: HMIDIIN) -> u32;
    fn midiInClose(hMidiIn: HMIDIIN) -> u32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiEvent {
    /// `channel` is 0-15, `value` 0-127
    ControlChange { channel: u8, controller: u8, value: u8 },
    NoteOn { channel: u8, note: u8, velocity: u8 },
}

/// What the WinMM callback gets as its instance pointer
struct MidiSink {
    tx: Sender<MidiEvent>,
    wake: Box<dyn Fn() + Send + Sync>,
}

/// WinMM calls this on its own thread when MIDI bytes arrive
unsafe extern "system" fn midi_in_callback(_h: HMIDIIN, w_msg: u32, dw_instance: usize, dw_param1: usize, _dw_param2: usize) {
    if w_msg != MIM_DATA || dw_instance == 0 {
        return;
    }
    let sink = &*(dw_instance as *const MidiSink);
    let status = (dw_param1 & 0xFF) as u8;
    let data1 = ((dw_param1 >> 8) & 0x7F) as u8;
    let data2 = ((dw_param1 >> 16) & 0x7F) as u8;
    let channel = status & 0x0F;
    let event = match status & 0xF0 {
        0xB0 => Some(MidiEvent::ControlChange { channel, controller: data1, value: data2 }),
        0x90 if data2 > 0 => Some(MidiEvent::NoteOn { channel, note: data1, velocity: data2 }),
        _ => None,
    };
    if let Some(e) = event {
        let _ = sink.tx.send(e);
        (sink.wake)();
    }
}

/// Listens to every MIDI input device for as long as it lives
pub struct MidiController {
    handles: Vec<HMIDIIN>,
    sink: *mut MidiSink,
}

// The sink is only freed in Drop, after every device is closed
unsafe impl Send for MidiController {}

impl MidiController {
    /// Open all inputs. `wake` is called after each event (e.g. to repaint the UI).
    pub fn open(wake: impl Fn() + Send + Sync + 'static) -> (Self, Receiver<MidiEvent>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let sink = Box::into_raw(Box::new(MidiSink { tx, wake: Box::new(wake) }));
        let mut handles = Vec::new();
        let num_devs = unsafe { midiInGetNumDevs() };
        println!("Hardware MIDI Subsystem: Detected {} MIDI Input Device(s)", num_devs);

        for dev_id in 0..num_devs {
            let mut handle: HMIDIIN = 0;
            let res = unsafe {
                midiInOpen(&mut handle, dev_id, midi_in_callback as *const () as usize, sink as usize, CALLBACK_FUNCTION)
            };
            if res == 0 && handle != 0 {
                if unsafe { midiInStart(handle) } == 0 {
                    handles.push(handle);
                } else {
                    unsafe { midiInClose(handle) };
                }
            }
        }
        (Self { handles, sink }, rx)
    }

    pub fn device_count(&self) -> usize {
        self.handles.len()
    }
}

impl Drop for MidiController {
    fn drop(&mut self) {
        for &handle in &self.handles {
            unsafe {
                let _ = midiInStop(handle);
                let _ = midiInClose(handle);
            }
        }
        self.handles.clear();
        // No callback can run once every device is closed
        unsafe { drop(Box::from_raw(self.sink)) };
    }
}
