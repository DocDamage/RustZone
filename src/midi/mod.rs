use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

// WinMM MIDI Type Definitions & Constants
pub type HMIDIIN = isize;
pub const CALLBACK_FUNCTION: u32 = 0x00030000;
pub const MIM_DATA: u32 = 0x3C3;
pub const MIM_OPEN: u32 = 0x3C1;
pub const MIM_CLOSE: u32 = 0x3C2;

pub const WM_APP_MIDI_CC: u32 = 0x8000 + 101;
pub const WM_APP_MIDI_NOTE: u32 = 0x8000 + 102;

#[link(name = "winmm")]
extern "system" {
    fn midiInGetNumDevs() -> u32;
    fn midiInOpen(
        lphMidiIn: *mut HMIDIIN,
        uDeviceID: u32,
        dwCallback: usize,
        dwInstance: usize,
        dwFlags: u32,
    ) -> u32;
    fn midiInStart(hMidiIn: HMIDIIN) -> u32;
    fn midiInStop(hMidiIn: HMIDIIN) -> u32;
    fn midiInClose(hMidiIn: HMIDIIN) -> u32;
}

static mut TARGET_HWND: Option<HWND> = None;

/// WinMM callback invoked by the Windows audio subsystem when incoming MIDI bytes arrive
unsafe extern "system" fn midi_in_callback(
    _h_midi_in: HMIDIIN,
    w_msg: u32,
    _dw_instance: usize,
    dw_param1: usize,
    _dw_param2: usize,
) {
    if w_msg == MIM_DATA {
        let status = (dw_param1 & 0xFF) as u8;
        let data1 = ((dw_param1 >> 8) & 0xFF) as u8;
        let data2 = ((dw_param1 >> 16) & 0xFF) as u8;

        if let Some(hwnd) = TARGET_HWND {
            let msg_type = status & 0xF0;
            if msg_type == 0xB0 {
                // Control Change (CC): WPARAM has (cc_num | (cc_val << 8)), LPARAM has status/channel
                let wparam = WPARAM((data1 as usize) | ((data2 as usize) << 8));
                let lparam = LPARAM(status as isize);
                let _ = PostMessageW(hwnd, WM_APP_MIDI_CC, wparam, lparam);
            } else if msg_type == 0x90 {
                // Note On: data1 = note, data2 = velocity
                if data2 > 0 {
                    let wparam = WPARAM((data1 as usize) | ((data2 as usize) << 8));
                    let lparam = LPARAM(status as isize);
                    let _ = PostMessageW(hwnd, WM_APP_MIDI_NOTE, wparam, lparam);
                }
            }
        }
    }
}

/// Hardware MIDI Controller Receiver
pub struct MidiController {
    handles: Vec<HMIDIIN>,
    is_active: Arc<AtomicBool>,
}

impl MidiController {
    pub fn new(target_hwnd: HWND) -> Self {
        unsafe {
            TARGET_HWND = Some(target_hwnd);
        }

        let mut handles = Vec::new();
        let num_devs = unsafe { midiInGetNumDevs() };
        println!("Hardware MIDI Subsystem: Detected {} MIDI Input Device(s)", num_devs);

        for dev_id in 0..num_devs {
            let mut handle: HMIDIIN = 0;
            let res = unsafe {
                midiInOpen(
                    &mut handle as *mut _,
                    dev_id,
                    midi_in_callback as *const () as usize,
                    0,
                    CALLBACK_FUNCTION,
                )
            };

            if res == 0 && handle != 0 {
                let start_res = unsafe { midiInStart(handle) };
                if start_res == 0 {
                    println!("   -> Connected and listening to MIDI Device #{}: Handle {:x}", dev_id, handle);
                    handles.push(handle);
                } else {
                    unsafe { midiInClose(handle) };
                }
            }
        }

        Self {
            handles,
            is_active: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn device_count(&self) -> usize {
        self.handles.len()
    }
}

impl Drop for MidiController {
    fn drop(&mut self) {
        self.is_active.store(false, Ordering::Relaxed);
        for &handle in &self.handles {
            unsafe {
                let _ = midiInStop(handle);
                let _ = midiInClose(handle);
            }
        }
        self.handles.clear();
        unsafe {
            TARGET_HWND = None;
        }
    }
}
