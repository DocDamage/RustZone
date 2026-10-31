//! Playback engine.
//!
//! The audio callback owns everything it processes (suite, buffers, plugin) and never waits on a
//! lock. The UI keeps its own copy of the suite for display and sends every change through a
//! lock-free command queue; whatever the audio thread replaces goes back through a second queue
//! so it is freed on the UI side. Meters come back through atomics and a snapshot the audio thread
//! only updates when nobody is reading it.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use parking_lot::Mutex;
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::buffer::AudioBuffer;
use super::codec_preview::{CodecPreview, PreviewMode};
use super::ml_separator::{self, StemBus, StemSet};
use super::montage::AlbumMontage;
use super::separator::StemSeparator;
use super::stems::StemMixer;
use super::trim::TrimFadeSettings;
use crate::dsp::{
    LoudnessFollower, MasterAssistant, MasterAssistantReport, MasteringSuite, MatchEq, ModuleKind, XmlPreset,
};
use crate::vst3::Vst3Plugin;
use std::path::Path;

const FP_ONE: f64 = 4294967296.0; // 2^32 for 32.32 fixed-point position

/// Largest block handed to a VST3 plugin in one `process` call; longer device callbacks are split
const PLUGIN_MAX_BLOCK: usize = 2048;

const COMMAND_QUEUE: usize = 1024;
const GARBAGE_QUEUE: usize = 1024;

/// Largest correction the loudness-matched A/B applies
const AB_MATCH_MAX_DB: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProcessingMode {
    Vst3,
    DspSuite,
    Bypass,
}

#[derive(Clone, Debug)]
pub struct MeterLevels {
    pub peak_l: f32,
    pub peak_r: f32,
    pub rms_l: f32,
    pub rms_r: f32,
    pub peak_l_db: f32,
    pub peak_r_db: f32,
    pub rms_l_db: f32,
    pub rms_r_db: f32,
    pub momentary_lufs: f32,
    pub short_term_lufs: f32,
    pub integrated_lufs: f32,
    pub true_peak_l_dbtp: f32,
    pub true_peak_r_dbtp: f32,
    pub phase_correlation: f32,
    pub spectrum_db: [f32; 128],
    /// Gain the loudness-matched A/B is applying to the processed signal (dB)
    pub ab_match_db: f32,
}

impl Default for MeterLevels {
    fn default() -> Self {
        Self {
            peak_l: 0.0,
            peak_r: 0.0,
            rms_l: 0.0,
            rms_r: 0.0,
            peak_l_db: -60.0,
            peak_r_db: -60.0,
            rms_l_db: -60.0,
            rms_r_db: -60.0,
            momentary_lufs: -70.0,
            short_term_lufs: -70.0,
            integrated_lufs: -70.0,
            true_peak_l_dbtp: -60.0,
            true_peak_r_dbtp: -60.0,
            phase_correlation: 1.0,
            spectrum_db: [-90.0; 128],
            ab_match_db: 0.0,
        }
    }
}

type SuiteEdit = Arc<dyn Fn(&mut MasteringSuite) + Send + Sync>;

/// UI → audio thread
enum Command {
    EditSuite(SuiteEdit),
    /// Swap in a fully configured suite (undo, snapshots, presets, sessions)
    ReplaceSuite(Box<MasteringSuite>),
    SetBuffer(Option<Arc<AudioBuffer>>),
    SetReference(Option<Arc<AudioBuffer>>),
    SetReferenceGain(f32),
    SetMode(ProcessingMode),
    SetTrimFade(TrimFadeSettings),
    SetPlugin(Option<Arc<Vst3Plugin>>),
    SetStems(Option<Arc<StemSet>>),
    SetCodecPreview(Option<Arc<CodecPreview>>),
    ResetProcessing,
}

/// Audio thread → UI: values the audio thread let go of, dropped on the UI side.
/// The payloads are never read; holding them is the point.
#[allow(dead_code)]
enum Garbage {
    Edit(SuiteEdit),
    Suite(Box<MasteringSuite>),
    Buffer(Arc<AudioBuffer>),
    Plugin(Arc<Vst3Plugin>),
    Stems(Arc<StemSet>),
    CodecPreview(Arc<CodecPreview>),
}

/// Readings the audio thread publishes; read with a short lock, written with `try_lock`
#[derive(Clone)]
struct MeterSnapshot {
    momentary_lufs: f32,
    short_term_lufs: f32,
    integrated_lufs: f32,
    true_peak_l_dbtp: f32,
    true_peak_r_dbtp: f32,
    phase_correlation: f32,
    spectrum_db: [f32; 128],
    module_meters: [[f32; 4]; ModuleKind::COUNT],
    auto_gain_db: [f32; ModuleKind::COUNT],
    ab_match_db: f32,
}

impl Default for MeterSnapshot {
    fn default() -> Self {
        Self {
            momentary_lufs: -70.0,
            short_term_lufs: -70.0,
            integrated_lufs: -70.0,
            true_peak_l_dbtp: -60.0,
            true_peak_r_dbtp: -60.0,
            phase_correlation: 1.0,
            spectrum_db: [-90.0; 128],
            module_meters: [[0.0; 4]; ModuleKind::COUNT],
            auto_gain_db: [0.0; ModuleKind::COUNT],
            ab_match_db: 0.0,
        }
    }
}

/// f32 stored in an AtomicU32
#[derive(Default)]
struct AtomicF32(AtomicU32);

impl AtomicF32 {
    fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// State both threads touch, all lock-free except the meter snapshot
struct Shared {
    cursor: AtomicU64,
    playing: AtomicBool,
    looping: AtomicBool,
    referencing: AtomicBool,
    ab_match: AtomicBool,
    /// `PreviewMode` as u8: what plays while a codec preview is loaded
    preview_mode: AtomicU8,
    /// Commands the audio thread has applied (compared against `UiState::commands_sent`)
    commands_applied: AtomicU64,
    peak_l: AtomicF32,
    peak_r: AtomicF32,
    rms_l: AtomicF32,
    rms_r: AtomicF32,
    snapshot: Mutex<MeterSnapshot>,
}

/// Everything the audio callback owns
struct RtState {
    suite: MasteringSuite,
    buffer: Option<Arc<AudioBuffer>>,
    reference: Option<Arc<AudioBuffer>>,
    reference_gain: f32,
    mode: ProcessingMode,
    trim_fade: TrimFadeSettings,
    plugin: Option<Arc<Vst3Plugin>>,
    stems: Option<Arc<StemSet>>,
    stem_bus: StemBus,
    codec_preview: Option<Arc<CodecPreview>>,
    commands: HeapCons<Command>,
    garbage: HeapProd<Garbage>,
    dry_loudness: LoudnessFollower,
    wet_loudness: LoudnessFollower,
    ab_gain: f32,
    frames_since_publish: usize,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    device_f32: Vec<f32>,
}

impl RtState {
    fn discard(&mut self, g: Garbage) {
        // If the UI isn't draining, freeing here is the lesser evil
        let _ = self.garbage.try_push(g);
    }

    fn reset_processing(&mut self) {
        self.suite.reset_state();
        self.stem_bus.reset();
        if let Some(ref plugin) = self.plugin {
            plugin.seek(0);
        }
        self.dry_loudness.reset();
        self.wet_loudness.reset();
        self.ab_gain = 1.0;
    }

    /// Apply queued commands; returns how many were applied
    fn drain_commands(&mut self) -> u64 {
        let mut applied = 0;
        while let Some(cmd) = self.commands.try_pop() {
            applied += 1;
            match cmd {
                Command::EditSuite(edit) => {
                    edit(&mut self.suite);
                    self.discard(Garbage::Edit(edit));
                }
                Command::ReplaceSuite(mut suite) => {
                    std::mem::swap(&mut self.suite, &mut suite);
                    self.discard(Garbage::Suite(suite));
                }
                Command::SetBuffer(buf) => {
                    if let Some(old) = std::mem::replace(&mut self.buffer, buf) {
                        self.discard(Garbage::Buffer(old));
                    }
                }
                Command::SetReference(buf) => {
                    if let Some(old) = std::mem::replace(&mut self.reference, buf) {
                        self.discard(Garbage::Buffer(old));
                    }
                }
                Command::SetReferenceGain(g) => self.reference_gain = g,
                Command::SetMode(m) => self.mode = m,
                Command::SetTrimFade(t) => self.trim_fade = t,
                Command::SetPlugin(p) => {
                    if let Some(old) = std::mem::replace(&mut self.plugin, p) {
                        self.discard(Garbage::Plugin(old));
                    }
                }
                Command::SetStems(s) => {
                    if let Some(old) = std::mem::replace(&mut self.stems, s) {
                        self.discard(Garbage::Stems(old));
                    }
                }
                Command::SetCodecPreview(p) => {
                    if let Some(old) = std::mem::replace(&mut self.codec_preview, p) {
                        self.discard(Garbage::CodecPreview(old));
                    }
                }
                Command::ResetProcessing => self.reset_processing(),
            }
        }
        applied
    }

    fn prepare(&mut self, num_frames: usize) {
        for buf in [&mut self.in_l, &mut self.in_r, &mut self.out_l, &mut self.out_r] {
            if buf.len() < num_frames {
                buf.resize(num_frames, 0.0);
            }
            buf[..num_frames].fill(0.0);
        }
    }

    /// Publish loudness, spectrum and module meters about 30 times a second
    fn publish(&mut self, shared: &Shared, num_frames: usize, device_sr: u32) {
        self.frames_since_publish += num_frames;
        if self.frames_since_publish < (device_sr as usize / 30).max(1) {
            return;
        }
        self.frames_since_publish = 0;
        if let Some(mut snap) = shared.snapshot.try_lock() {
            self.suite.spectral_analyzer.update_spectrum();
            let r = self.suite.lufs_meter.get_readout();
            snap.momentary_lufs = r.momentary_lufs;
            snap.short_term_lufs = r.short_term_lufs;
            snap.integrated_lufs = r.integrated_lufs;
            snap.true_peak_l_dbtp = r.true_peak_l_dbtp;
            snap.true_peak_r_dbtp = r.true_peak_r_dbtp;
            snap.phase_correlation = r.phase_correlation;
            snap.spectrum_db = *self.suite.spectral_analyzer.get_spectrum_db();
            for &kind in ModuleKind::ALL {
                snap.module_meters[kind.index()] = self.suite.module(kind).meters();
                snap.auto_gain_db[kind.index()] = self.suite.auto_gain_db(kind);
            }
            snap.ab_match_db = 20.0 * self.ab_gain.max(1e-6).log10();
        }
    }
}

/// UI-side model: what the UI shows and edits. Never touched by the audio thread.
struct UiState {
    suite: MasteringSuite,
    audio_buffer: Option<Arc<AudioBuffer>>,
    reference_buffer: Option<Arc<AudioBuffer>>,
    reference_gain_offset: f32,
    plugin: Option<Arc<Vst3Plugin>>,
    mode: ProcessingMode,
    trim_fade: TrimFadeSettings,
    montage: AlbumMontage,
    in_montage_mode: bool,
    /// Single-track buffer parked while the album montage is loaded for playback
    single_track_buffer: Option<Arc<AudioBuffer>>,
    stem_mixer: StemMixer,
    /// Trained-model stems and the exact buffer they were separated from
    ml_stems: Option<(Arc<AudioBuffer>, Arc<StemSet>)>,
    /// Codec preview of the loaded buffer (cleared whenever the buffer changes)
    codec_preview: Option<Arc<CodecPreview>>,
    commands: HeapProd<Command>,
    commands_sent: u64,
}

impl UiState {
    /// Buffer the transport is currently playing (reference track while auditioning it)
    fn active_buffer(&self, referencing: bool) -> Option<&Arc<AudioBuffer>> {
        if referencing && self.reference_buffer.is_some() {
            self.reference_buffer.as_ref()
        } else {
            self.audio_buffer.as_ref()
        }
    }

    /// Queue a command for the audio thread, returning its sequence number
    fn send(&mut self, cmd: Command) -> u64 {
        let mut cmd = cmd;
        let started = Instant::now();
        loop {
            match self.commands.try_push(cmd) {
                Ok(()) => break,
                Err(back) => {
                    // Queue full: the audio thread is behind or stopped; wait briefly, then drop
                    if started.elapsed() > Duration::from_millis(250) {
                        eprintln!("Audio command queue is full; dropping a command");
                        return self.commands_sent;
                    }
                    cmd = back;
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        self.commands_sent += 1;
        self.commands_sent
    }
}

/// What the output stream is running on
#[derive(Clone, Debug, Default)]
struct DeviceDesc {
    host: String,
    name: String,
    sample_rate: u32,
    channels: usize,
    sample_format: String,
    buffer_frames: Option<u32>,
}

/// Which output to open. `None` fields mean "the host's/device's default".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceChoice {
    pub host: Option<String>,
    pub device: Option<String>,
    pub sample_rate: Option<u32>,
    /// Requested callback size in frames (devices may round it)
    pub buffer_frames: Option<u32>,
}

/// An output device and what it supports
#[derive(Clone, Debug)]
pub struct OutputDeviceInfo {
    pub name: String,
    pub default_sample_rate: u32,
    /// Common rates the device accepts for stereo float/int16 output
    pub sample_rates: Vec<u32>,
    /// Supported callback size range in frames, when the driver reports one
    pub buffer_range: Option<(u32, u32)>,
}

pub struct AudioEngine {
    stream: Mutex<Option<Stream>>,
    ui: Mutex<UiState>,
    shared: Arc<Shared>,
    garbage: Mutex<HeapCons<Garbage>>,
    device: Mutex<DeviceDesc>,
    device_sample_rate: AtomicU32,
    /// Bumped on every montage rebuild so a slower, older rebuild can't overwrite a newer one
    montage_generation: AtomicU64,
    separating: AtomicBool,
    separation_progress: AtomicF32,
}

/// Where trained stem separation stands for the loaded track
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StemStatus {
    /// No model file found
    Unavailable,
    /// Model present, track not separated yet
    NotSeparated,
    Separating(f32),
    Ready,
}

#[derive(Clone, Debug)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub host: String,
    pub sample_rate: u32,
    pub channels: usize,
    pub sample_format: String,
    /// Callback size in frames, when one was requested
    pub buffer_frames: Option<u32>,
    pub latency_ms: f32,
    pub available_devices: Vec<String>,
}

// The cpal stream is only touched while holding its mutex
unsafe impl Send for AudioEngine {}
unsafe impl Sync for AudioEngine {}

const COMMON_RATES: [u32; 6] = [44100, 48000, 88200, 96000, 176400, 192000];

/// Audio host APIs compiled in (WASAPI by default; ASIO with the `asio` feature)
pub fn list_audio_hosts() -> Vec<String> {
    cpal::available_hosts().into_iter().map(|id| id.name().to_string()).collect()
}

fn find_host(name: Option<&str>) -> Result<cpal::Host, String> {
    match name {
        None => Ok(cpal::default_host()),
        Some(n) => {
            let id = cpal::available_hosts()
                .into_iter()
                .find(|id| id.name().eq_ignore_ascii_case(n))
                .ok_or_else(|| format!("Audio host '{}' is not available", n))?;
            cpal::host_from_id(id).map_err(|e| format!("Could not open audio host '{}': {}", n, e))
        }
    }
}

/// Output devices of a host (default host when `None`)
pub fn list_output_devices(host: Option<&str>) -> Vec<OutputDeviceInfo> {
    let Ok(host) = find_host(host) else { return Vec::new() };
    let Ok(devices) = host.output_devices() else { return Vec::new() };
    devices
        .filter_map(|d| {
            let name = d.name().ok()?;
            let default_sample_rate = d.default_output_config().map(|c| c.sample_rate().0).unwrap_or(48000);
            let mut sample_rates = Vec::new();
            let mut buffer_range = None;
            if let Ok(configs) = d.supported_output_configs() {
                for range in configs.filter(|r| r.channels() >= 2) {
                    for &rate in &COMMON_RATES {
                        if rate >= range.min_sample_rate().0 && rate <= range.max_sample_rate().0 && !sample_rates.contains(&rate) {
                            sample_rates.push(rate);
                        }
                    }
                    if let cpal::SupportedBufferSize::Range { min, max } = range.buffer_size() {
                        buffer_range = Some((*min, *max));
                    }
                }
            }
            sample_rates.sort_unstable();
            Some(OutputDeviceInfo { name, default_sample_rate, sample_rates, buffer_range })
        })
        .collect()
}

/// Pick the device and stream configuration for a choice
fn resolve_output(choice: &DeviceChoice) -> Result<(cpal::Device, StreamConfig, SampleFormat, DeviceDesc), String> {
    let host = find_host(choice.host.as_deref())?;
    let device = match &choice.device {
        None => host.default_output_device().ok_or("No default audio output device available")?,
        Some(name) => host
            .output_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.name().map(|n| &n == name).unwrap_or(false))
            .ok_or_else(|| format!("Audio device '{}' not found", name))?,
    };
    let default = device
        .default_output_config()
        .map_err(|e| format!("Failed to query audio output config: {}", e))?;
    let mut sample_format = default.sample_format();
    let mut config: StreamConfig = default.into();

    if let Some(rate) = choice.sample_rate {
        // Find a supported range at this rate, preferring float output
        let ranges: Vec<_> = device
            .supported_output_configs()
            .map_err(|e| e.to_string())?
            .filter(|r| r.channels() >= 2 && rate >= r.min_sample_rate().0 && rate <= r.max_sample_rate().0)
            .filter(|r| matches!(r.sample_format(), SampleFormat::F32 | SampleFormat::I16))
            .collect();
        let range = ranges
            .iter()
            .find(|r| r.sample_format() == SampleFormat::F32)
            .or_else(|| ranges.first())
            .ok_or_else(|| format!("The device does not support {} Hz", rate))?;
        sample_format = range.sample_format();
        config.channels = range.channels();
        config.sample_rate = cpal::SampleRate(rate);
    }
    if let Some(frames) = choice.buffer_frames {
        config.buffer_size = cpal::BufferSize::Fixed(frames);
    }

    let desc = DeviceDesc {
        host: host.id().name().to_string(),
        name: device.name().unwrap_or_else(|_| "Output".to_string()),
        sample_rate: config.sample_rate.0,
        channels: config.channels as usize,
        sample_format: format!("{:?}", sample_format),
        buffer_frames: choice.buffer_frames,
    };
    Ok((device, config, sample_format, desc))
}

fn new_rt_state(ui: &UiState, sr: f32, commands: HeapCons<Command>, garbage: HeapProd<Garbage>) -> RtState {
    RtState {
        suite: ui.suite.clone(),
        buffer: ui.audio_buffer.clone(),
        reference: ui.reference_buffer.clone(),
        reference_gain: ui.reference_gain_offset,
        mode: ui.mode,
        trim_fade: ui.trim_fade,
        plugin: ui.plugin.clone(),
        stems: match (&ui.ml_stems, &ui.audio_buffer) {
            (Some((src, stems)), Some(buf)) if Arc::ptr_eq(src, buf) => Some(Arc::clone(stems)),
            _ => None,
        },
        stem_bus: StemBus::new(sr),
        codec_preview: ui.codec_preview.clone(),
        commands,
        garbage,
        dry_loudness: LoudnessFollower::new(sr, 3.0),
        wet_loudness: LoudnessFollower::new(sr, 3.0),
        ab_gain: 1.0,
        frames_since_publish: 0,
        in_l: vec![0.0; 8192],
        in_r: vec![0.0; 8192],
        out_l: vec![0.0; 8192],
        out_r: vec![0.0; 8192],
        device_f32: vec![0.0; 16384],
    }
}

fn open_stream(device: &cpal::Device, config: &StreamConfig, sample_format: SampleFormat, shared: &Arc<Shared>, rt: RtState) -> Result<Stream, String> {
    let channels = config.channels as usize;
    let device_sample_rate = config.sample_rate.0;
    let err_fn = |err| eprintln!("Audio stream error: {}", err);
    // The callback closure owns `rt`; nothing else can reach it
    let mut rt = rt;
    let stream = match sample_format {
        SampleFormat::F32 => {
            let shared_cb = Arc::clone(shared);
            device
                .build_output_stream(
                    config,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        AudioEngine::process_audio_f32(data, &mut rt, &shared_cb, channels, device_sample_rate);
                    },
                    err_fn,
                    None,
                )
                .map_err(|e| format!("Failed to build F32 output stream: {}", e))?
        }
        SampleFormat::I16 => {
            let shared_cb = Arc::clone(shared);
            device
                .build_output_stream(
                    config,
                    move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                        let mut f32_buf = std::mem::take(&mut rt.device_f32);
                        if f32_buf.len() < data.len() {
                            f32_buf.resize(data.len(), 0.0);
                        }
                        AudioEngine::process_audio_f32(&mut f32_buf[..data.len()], &mut rt, &shared_cb, channels, device_sample_rate);
                        for (out, &s) in data.iter_mut().zip(f32_buf.iter()) {
                            *out = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                        }
                        rt.device_f32 = f32_buf;
                    },
                    err_fn,
                    None,
                )
                .map_err(|e| format!("Failed to build I16 output stream: {}", e))?
        }
        _ => return Err(format!("Unsupported audio sample format: {:?}", sample_format)),
    };
    stream.play().map_err(|e| format!("Failed to start audio stream: {}", e))?;
    Ok(stream)
}

impl AudioEngine {
    /// Open the default output device
    pub fn new() -> Result<Self, String> {
        Self::with_device(&DeviceChoice::default())
    }

    pub fn with_device(choice: &DeviceChoice) -> Result<Self, String> {
        let (device, config, sample_format, desc) = resolve_output(choice)?;
        let sr = desc.sample_rate as f32;

        let (cmd_prod, cmd_cons) = HeapRb::<Command>::new(COMMAND_QUEUE).split();
        let (garbage_prod, garbage_cons) = HeapRb::<Garbage>::new(GARBAGE_QUEUE).split();

        let shared = Arc::new(Shared {
            cursor: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            looping: AtomicBool::new(false),
            referencing: AtomicBool::new(false),
            ab_match: AtomicBool::new(false),
            preview_mode: AtomicU8::new(0),
            commands_applied: AtomicU64::new(0),
            peak_l: AtomicF32::default(),
            peak_r: AtomicF32::default(),
            rms_l: AtomicF32::default(),
            rms_r: AtomicF32::default(),
            snapshot: Mutex::new(MeterSnapshot::default()),
        });

        // Both copies start identical; afterwards every edit is applied to both
        let ui = UiState {
            suite: MasteringSuite::new(sr),
            audio_buffer: None,
            reference_buffer: None,
            reference_gain_offset: 1.0,
            plugin: None,
            mode: ProcessingMode::DspSuite,
            trim_fade: TrimFadeSettings::default(),
            montage: AlbumMontage::default(),
            in_montage_mode: false,
            single_track_buffer: None,
            stem_mixer: StemMixer::default(),
            ml_stems: None,
            codec_preview: None,
            commands: cmd_prod,
            commands_sent: 0,
        };
        let rt = new_rt_state(&ui, sr, cmd_cons, garbage_prod);
        let stream = open_stream(&device, &config, sample_format, &shared, rt)?;

        Ok(Self {
            stream: Mutex::new(Some(stream)),
            ui: Mutex::new(ui),
            shared,
            garbage: Mutex::new(garbage_cons),
            device_sample_rate: AtomicU32::new(desc.sample_rate),
            device: Mutex::new(desc),
            montage_generation: AtomicU64::new(0),
            separating: AtomicBool::new(false),
            separation_progress: AtomicF32::default(),
        })
    }

    /// Switch to another host / device / sample rate / buffer size. Playback stops; the track,
    /// suite settings, plugin and stems carry over. On failure the previous output is reopened.
    pub fn reopen(&self, choice: &DeviceChoice) -> Result<(), String> {
        let previous = {
            let d = self.device.lock();
            DeviceChoice {
                host: Some(d.host.clone()),
                device: Some(d.name.clone()),
                sample_rate: Some(d.sample_rate),
                buffer_frames: d.buffer_frames,
            }
        };
        match self.reopen_inner(choice) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = self.reopen_inner(&previous).or_else(|_| self.reopen_inner(&DeviceChoice::default()));
                Err(e)
            }
        }
    }

    fn reopen_inner(&self, choice: &DeviceChoice) -> Result<(), String> {
        self.shared.playing.store(false, Ordering::Relaxed);
        // Stop the old stream first; dropping it drops its callback state here, on this thread
        drop(self.stream.lock().take());
        self.collect_garbage();

        let (device, config, sample_format, desc) = resolve_output(choice)?;
        let sr = desc.sample_rate;

        let (cmd_prod, cmd_cons) = HeapRb::<Command>::new(COMMAND_QUEUE).split();
        let (garbage_prod, garbage_cons) = HeapRb::<Garbage>::new(GARBAGE_QUEUE).split();
        let rt = {
            let mut ui = self.ui.lock();
            if ui.suite.sample_rate as u32 != sr {
                ui.suite.set_sample_rate(sr as f32);
            }
            if let Some(ref p) = ui.plugin {
                p.setup_audio(sr as f64, PLUGIN_MAX_BLOCK)?;
            }
            ui.commands = cmd_prod;
            ui.commands_sent = 0;
            self.shared.commands_applied.store(0, Ordering::Release);
            new_rt_state(&ui, sr as f32, cmd_cons, garbage_prod)
        };
        *self.garbage.lock() = garbage_cons;

        let stream = open_stream(&device, &config, sample_format, &self.shared, rt)?;
        *self.stream.lock() = Some(stream);
        self.device_sample_rate.store(sr, Ordering::Release);
        *self.device.lock() = desc;
        Ok(())
    }

    /// The output currently in use, as a choice that `reopen` accepts
    pub fn current_device(&self) -> DeviceChoice {
        let d = self.device.lock();
        DeviceChoice {
            host: Some(d.host.clone()),
            device: Some(d.name.clone()),
            sample_rate: Some(d.sample_rate),
            buffer_frames: d.buffer_frames,
        }
    }

    fn process_audio_f32(output: &mut [f32], rt: &mut RtState, shared: &Shared, num_device_channels: usize, device_sr: u32) {
        let num_frames = output.len() / num_device_channels.max(1);
        output.fill(0.0);

        let applied = rt.drain_commands();
        if applied > 0 {
            shared.commands_applied.fetch_add(applied, Ordering::Release);
        }
        if num_frames == 0 {
            return;
        }

        if !shared.playing.load(Ordering::Relaxed) {
            // Decay meters smoothly when stopped
            for m in [&shared.peak_l, &shared.peak_r, &shared.rms_l, &shared.rms_r] {
                m.store(m.load() * 0.85);
            }
            rt.publish(shared, num_frames, device_sr);
            return;
        }

        rt.prepare(num_frames);
        let looping = shared.looping.load(Ordering::Relaxed);
        let referencing = shared.referencing.load(Ordering::Relaxed) && rt.reference.is_some();
        let mut reached_end = false;
        let mut cur_fixed = shared.cursor.load(Ordering::Relaxed);

        // Split borrows: the scratch buffers and the rest of the state are separate fields
        let preview_mode = shared.preview_mode.load(Ordering::Relaxed);
        let RtState { suite, buffer, reference, reference_gain, mode, trim_fade, plugin, stems, stem_bus, codec_preview, dry_loudness, wet_loudness, ab_gain, in_l, in_r, out_l, out_r, .. } = rt;
        let (in_l, in_r) = (&mut in_l[..num_frames], &mut in_r[..num_frames]);
        let (out_l, out_r) = (&mut out_l[..num_frames], &mut out_r[..num_frames]);

        if referencing {
            if let Some(ref_buf) = reference.as_deref() {
                let total_frames = ref_buf.num_frames();
                if total_frames > 0 {
                    let step = ((ref_buf.sample_rate as f64 / device_sr as f64) * FP_ONE) as u64;
                    for i in 0..num_frames {
                        if (cur_fixed >> 32) as usize >= total_frames {
                            if looping {
                                cur_fixed = 0;
                            } else {
                                reached_end = true;
                                break;
                            }
                        }
                        let (s_l, s_r) = ref_buf.get_interpolated_frame(cur_fixed as f64 / FP_ONE);
                        out_l[i] = s_l * *reference_gain;
                        out_r[i] = s_r * *reference_gain;
                        cur_fixed += step;
                    }
                }
            }
            suite.lufs_meter.process_block(out_l, out_r);
            suite.spectral_analyzer.feed_samples(out_l, out_r);
        } else {
            let mut previewing: Option<&AudioBuffer> = None;
            if let Some(buf) = buffer.as_deref() {
                let total_frames = buf.num_frames();
                if total_frames > 0 {
                    let (start_frame, end_frame) = trim_fade.get_frame_boundaries(total_frames);
                    let cur_idx = (cur_fixed >> 32) as usize;
                    if cur_idx < start_frame || cur_idx >= end_frame {
                        cur_fixed = (start_frame as u64) << 32;
                    }
                    // Fractional step per device output frame for pitch-correct playback
                    let step = ((buf.sample_rate as f64 / device_sr as f64) * FP_ONE) as u64;
                    // A codec preview of this buffer plays the encoded master (or what the codec
                    // removed) in place of the live chain
                    previewing = codec_preview
                        .as_deref()
                        .filter(|p| p.decoded.num_frames() == total_frames)
                        .and_then(|p| match preview_mode {
                            1 => Some(&p.decoded),
                            2 => Some(&p.difference),
                            _ => None,
                        });
                    // With trained stems for this buffer, Stem Rebalance / Stem EQ act on the real
                    // stems (mixed here, in front of the chain) instead of the spectral approximation
                    let stem_src = stems
                        .as_deref()
                        .filter(|s| s.num_frames == total_frames && matches!(*mode, ProcessingMode::DspSuite) && suite.uses_stems());
                    suite.external_stems = stem_src.is_some();
                    if stem_src.is_some() {
                        stem_bus.configure(suite);
                    }
                    for i in 0..num_frames {
                        let cur_frame_idx = (cur_fixed >> 32) as usize;
                        if cur_frame_idx >= end_frame {
                            if looping {
                                cur_fixed = (start_frame as u64) << 32;
                            } else {
                                reached_end = true;
                                break;
                            }
                        }
                        let pos = cur_fixed as f64 / FP_ONE;
                        let (s_l, s_r) = match (previewing, stem_src) {
                            (Some(p), _) => p.get_interpolated_frame(pos),
                            (None, Some(st)) => stem_bus.mix(st.frames_at(pos)),
                            (None, None) => buf.get_interpolated_frame(pos),
                        };
                        let fade = trim_fade.calculate_gain(cur_frame_idx, start_frame, end_frame, buf.sample_rate);
                        in_l[i] = s_l * fade;
                        in_r[i] = s_r * fade;
                        cur_fixed += step;
                    }
                }
            }

            out_l.copy_from_slice(in_l);
            out_r.copy_from_slice(in_r);
            let processed = match *mode {
                // The preview is already the finished master
                _ if previewing.is_some() => false,
                ProcessingMode::Vst3 => {
                    if let Some(p) = plugin.as_deref() {
                        p.process_stereo(in_l, in_r, out_l, out_r);
                        true
                    } else {
                        false
                    }
                }
                ProcessingMode::DspSuite => {
                    suite.process_stereo(out_l, out_r);
                    true
                }
                ProcessingMode::Bypass => false,
            };
            if !matches!(*mode, ProcessingMode::DspSuite) || previewing.is_some() {
                suite.lufs_meter.process_block(out_l, out_r);
                suite.spectral_analyzer.feed_samples(out_l, out_r);
            }

            // Loudness-matched A/B: track input vs processed loudness, and when enabled play the
            // processed signal at the input's loudness so comparisons aren't biased by level
            if processed {
                dry_loudness.feed(in_l, in_r);
                wet_loudness.feed(out_l, out_r);
                let target = if shared.ab_match.load(Ordering::Relaxed) && dry_loudness.is_active() && wet_loudness.is_active() {
                    let db = (dry_loudness.lufs() - wet_loudness.lufs()).clamp(-AB_MATCH_MAX_DB, AB_MATCH_MAX_DB);
                    10.0f32.powf(db / 20.0)
                } else {
                    1.0
                };
                let c = (-(num_frames as f32) / (0.2 * device_sr as f32)).exp();
                let next = target + c * (*ab_gain - target);
                let step = (next - *ab_gain) / num_frames as f32;
                if (next - 1.0).abs() > 1e-6 || (*ab_gain - 1.0).abs() > 1e-6 {
                    for i in 0..num_frames {
                        let g = *ab_gain + step * (i + 1) as f32;
                        out_l[i] *= g;
                        out_r[i] *= g;
                    }
                }
                *ab_gain = next;
            }
        }

        shared.cursor.store(cur_fixed, Ordering::Relaxed);
        if reached_end {
            shared.playing.store(false, Ordering::Relaxed);
        }

        // Peak / RMS metering and interleave into the device buffer
        let (mut max_l, mut max_r, mut sum_l, mut sum_r) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for i in 0..num_frames {
            let (sl, sr) = (out_l[i], out_r[i]);
            max_l = max_l.max(sl.abs());
            max_r = max_r.max(sr.abs());
            sum_l += sl * sl;
            sum_r += sr * sr;
            output[i * num_device_channels] = sl;
            if num_device_channels > 1 {
                output[i * num_device_channels + 1] = sr;
            }
        }
        let rms_l = (sum_l / num_frames as f32).sqrt();
        let rms_r = (sum_r / num_frames as f32).sqrt();
        shared.peak_l.store(shared.peak_l.load().max(max_l) * 0.92);
        shared.peak_r.store(shared.peak_r.load().max(max_r) * 0.92);
        shared.rms_l.store(shared.rms_l.load().max(rms_l) * 0.92);
        shared.rms_r.store(shared.rms_r.load().max(rms_r) * 0.92);

        rt.publish(shared, num_frames, device_sr);
    }

    /// Wait (briefly) until the audio thread has applied command number `seq`. Returns false on
    /// timeout, which means the callback isn't running and so can't be using anything anyway.
    fn wait_applied(&self, seq: u64) -> bool {
        let started = Instant::now();
        while self.shared.commands_applied.load(Ordering::Acquire) < seq {
            if started.elapsed() > Duration::from_millis(500) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    }

    /// Wait until the audio thread has applied every command sent so far. Returns false if it
    /// didn't within half a second (stream stopped or device gone).
    pub fn flush_commands(&self) -> bool {
        let seq = self.ui.lock().commands_sent;
        self.wait_applied(seq)
    }

    /// Free whatever the audio thread released. Call regularly from the UI thread (plugins are
    /// dropped here, so their editor teardown happens on the UI thread).
    pub fn collect_garbage(&self) {
        let mut garbage = self.garbage.lock();
        while let Some(g) = garbage.try_pop() {
            drop(g);
        }
    }

    pub fn device_sample_rate(&self) -> u32 {
        self.device_sample_rate.load(Ordering::Acquire)
    }

    pub fn play(&self) {
        self.shared.playing.store(true, Ordering::Relaxed);
    }

    pub fn pause(&self) {
        self.shared.playing.store(false, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.shared.playing.store(false, Ordering::Relaxed);
        self.shared.cursor.store(0, Ordering::Relaxed);
        self.ui.lock().send(Command::ResetProcessing);
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    pub fn set_loop(&self, enabled: bool) {
        self.shared.looping.store(enabled, Ordering::Relaxed);
    }

    pub fn is_looping(&self) -> bool {
        self.shared.looping.load(Ordering::Relaxed)
    }

    pub fn seek_frame(&self, frame: usize) {
        self.shared.cursor.store((frame as u64) << 32, Ordering::Relaxed);
    }

    pub fn seek_fraction(&self, fraction: f64) {
        let frames = self.ui.lock().active_buffer(self.is_referencing()).map(|b| b.num_frames());
        if let Some(n) = frames {
            self.seek_frame((n as f64 * fraction.clamp(0.0, 1.0)) as usize);
        }
    }

    pub fn current_frame(&self) -> usize {
        (self.shared.cursor.load(Ordering::Relaxed) >> 32) as usize
    }

    pub fn current_position_seconds(&self) -> f64 {
        let sr = self.ui.lock().active_buffer(self.is_referencing()).map(|b| b.sample_rate);
        match sr {
            Some(sr) if sr > 0 => (self.shared.cursor.load(Ordering::Relaxed) as f64 / FP_ONE) / sr as f64,
            _ => 0.0,
        }
    }

    pub fn duration_seconds(&self) -> f64 {
        self.ui.lock().active_buffer(self.is_referencing()).map(|b| b.duration_seconds()).unwrap_or(0.0)
    }

    /// Load a new track: rewinds the transport and clears processing state from the previous track.
    /// The DSP suite stays at the device rate because playback is resampled before processing.
    pub fn set_audio_buffer(&self, buffer: AudioBuffer) {
        let buffer = Arc::new(buffer);
        let mut ui = self.ui.lock();
        if ui.in_montage_mode {
            // A track opened while the montage is loaded replaces the parked single track
            ui.in_montage_mode = false;
            ui.single_track_buffer = None;
        }
        ui.audio_buffer = Some(Arc::clone(&buffer));
        self.shared.cursor.store(0, Ordering::Relaxed);
        ui.send(Command::SetBuffer(Some(buffer)));
        Self::clear_ml_stems(&mut ui);
        self.clear_codec_preview(&mut ui);
        ui.send(Command::ResetProcessing);
    }

    fn clear_ml_stems(ui: &mut UiState) {
        if ui.ml_stems.take().is_some() {
            ui.send(Command::SetStems(None));
        }
    }

    /// Where trained stem separation stands for the loaded track
    pub fn stem_status(&self) -> StemStatus {
        if self.separating.load(Ordering::Relaxed) {
            return StemStatus::Separating(self.separation_progress.load());
        }
        let ui = self.ui.lock();
        let current = match (&ui.ml_stems, &ui.audio_buffer) {
            (Some((src, _)), Some(buf)) => Arc::ptr_eq(src, buf),
            _ => false,
        };
        if current {
            StemStatus::Ready
        } else if ml_separator::find_model().is_some() {
            StemStatus::NotSeparated
        } else {
            StemStatus::Unavailable
        }
    }

    /// Trained-model stems of the loaded track, if it has been separated
    pub fn ml_stems(&self) -> Option<Arc<StemSet>> {
        let ui = self.ui.lock();
        match (&ui.ml_stems, &ui.audio_buffer) {
            (Some((src, stems)), Some(buf)) if Arc::ptr_eq(src, buf) => Some(Arc::clone(stems)),
            _ => None,
        }
    }

    /// Separate the loaded track with the bundled model (blocking: run it on a worker thread).
    /// When it finishes, playback and exports use the stems for Stem Rebalance / Stem EQ, and
    /// the stems are returned for other uses (the stem mixer).
    pub fn separate_with_model(&self, progress: impl Fn(f32)) -> Result<Arc<StemSet>, String> {
        if self.separating.swap(true, Ordering::AcqRel) {
            return Err("Stem separation is already running".to_string());
        }
        self.separation_progress.store(0.0);
        let result = (|| {
            let source = self.audio_buffer().ok_or("Load a track first")?;
            let stems = ml_separator::separate_with_bundled_model(&source, |p| {
                self.separation_progress.store(p);
                progress(p);
            })?;
            let mut ui = self.ui.lock();
            // Only attach if the same track is still loaded
            if ui.audio_buffer.as_ref().map(|b| Arc::ptr_eq(b, &source)).unwrap_or(false) {
                ui.ml_stems = Some((source, Arc::clone(&stems)));
                ui.send(Command::SetStems(Some(Arc::clone(&stems))));
            }
            Ok(stems)
        })();
        self.separating.store(false, Ordering::Release);
        result
    }

    /// Swap the playing buffer without rewinding (stem remixes, montage rebuilds)
    fn replace_audio_buffer_keep_position(&self, buffer: Option<Arc<AudioBuffer>>) {
        let mut ui = self.ui.lock();
        let max_fixed = (buffer.as_ref().map(|b| b.num_frames()).unwrap_or(0) as u64) << 32;
        if self.shared.cursor.load(Ordering::Relaxed) > max_fixed {
            self.shared.cursor.store(0, Ordering::Relaxed);
        }
        ui.audio_buffer = buffer.clone();
        ui.send(Command::SetBuffer(buffer));
        Self::clear_ml_stems(&mut ui);
        self.clear_codec_preview(&mut ui);
    }

    /// Shared handle to the loaded track (cheap; no copy)
    pub fn audio_buffer(&self) -> Option<Arc<AudioBuffer>> {
        self.ui.lock().audio_buffer.clone()
    }

    pub fn with_audio_buffer<R>(&self, f: impl FnOnce(Option<&AudioBuffer>) -> R) -> R {
        let buf = self.audio_buffer();
        f(buf.as_deref())
    }

    pub fn set_reference_buffer(&self, buffer: AudioBuffer) {
        let buffer = Arc::new(buffer);
        let mut ui = self.ui.lock();
        ui.reference_buffer = Some(Arc::clone(&buffer));
        ui.send(Command::SetReference(Some(buffer)));
    }

    pub fn reference_buffer(&self) -> Option<Arc<AudioBuffer>> {
        self.ui.lock().reference_buffer.clone()
    }

    pub fn with_reference_buffer<R>(&self, f: impl FnOnce(Option<&AudioBuffer>) -> R) -> R {
        let buf = self.reference_buffer();
        f(buf.as_deref())
    }

    /// Load (or clear) a codec preview of the current buffer. Playback switches to it with
    /// `set_preview_mode`.
    pub fn set_codec_preview(&self, preview: Option<CodecPreview>) {
        let preview = preview.map(Arc::new);
        let mut ui = self.ui.lock();
        if preview.is_none() {
            self.shared.preview_mode.store(0, Ordering::Relaxed);
        }
        ui.codec_preview = preview.clone();
        ui.send(Command::SetCodecPreview(preview));
    }

    pub fn codec_preview(&self) -> Option<Arc<CodecPreview>> {
        self.ui.lock().codec_preview.clone()
    }

    fn clear_codec_preview(&self, ui: &mut UiState) {
        if ui.codec_preview.take().is_some() {
            self.shared.preview_mode.store(0, Ordering::Relaxed);
            ui.send(Command::SetCodecPreview(None));
        }
    }

    /// Switch playback between the live chain and the loaded codec preview
    pub fn set_preview_mode(&self, mode: PreviewMode) {
        let mut ui = self.ui.lock();
        let mode = if ui.codec_preview.is_some() { mode } else { PreviewMode::Off };
        let code = match mode {
            PreviewMode::Off => 0,
            PreviewMode::Codec => 1,
            PreviewMode::Difference => 2,
        };
        if self.shared.preview_mode.swap(code, Ordering::Relaxed) != code {
            // The chain sat idle while the preview played: don't resume on stale filter state
            ui.send(Command::ResetProcessing);
        }
    }

    pub fn preview_mode(&self) -> PreviewMode {
        match self.shared.preview_mode.load(Ordering::Relaxed) {
            1 => PreviewMode::Codec,
            2 => PreviewMode::Difference,
            _ => PreviewMode::Off,
        }
    }

    pub fn set_referencing(&self, enabled: bool) {
        self.shared.referencing.store(enabled, Ordering::Relaxed);
    }

    pub fn is_referencing(&self) -> bool {
        self.shared.referencing.load(Ordering::Relaxed)
    }

    pub fn set_reference_gain_offset(&self, offset: f32) {
        let offset = offset.clamp(0.01, 10.0);
        let mut ui = self.ui.lock();
        ui.reference_gain_offset = offset;
        ui.send(Command::SetReferenceGain(offset));
    }

    pub fn reference_gain_offset(&self) -> f32 {
        self.ui.lock().reference_gain_offset
    }

    /// Auto-matches reference track loudness to main track via delta-LUFS gain offset
    pub fn auto_match_reference_gain(&self) -> Result<(f32, f32, f32), String> {
        let main_buf = self.audio_buffer().ok_or("No main track loaded to match against")?;
        let ref_buf = self.reference_buffer().ok_or("No reference track loaded")?;

        let main_lufs = crate::dsp::lufs::calculate_buffer_integrated_lufs(&main_buf);
        let ref_lufs = crate::dsp::lufs::calculate_buffer_integrated_lufs(&ref_buf);

        if main_lufs <= -69.0 || ref_lufs <= -69.0 {
            return Err("Signal level too low to accurately compute LUFS".to_string());
        }

        let delta_db = main_lufs - ref_lufs;
        self.set_reference_gain_offset(10.0f32.powf(delta_db / 20.0));
        Ok((main_lufs, ref_lufs, delta_db))
    }

    /// Match Maximizer parameters to target streaming standard
    pub fn apply_loudness_target(&self, target_lufs: f32, target_ceiling_dbtp: f32) -> Result<(f32, f32), String> {
        let main_buf = self.audio_buffer().ok_or("No main track loaded")?;
        let current_lufs = crate::dsp::lufs::calculate_buffer_integrated_lufs(&main_buf);

        if current_lufs <= -69.0 {
            return Err("Signal level too low to compute target drive".to_string());
        }

        let needed_boost = (target_lufs - current_lufs).max(0.0);
        // Apply drive to maximizer threshold (negative threshold = drive)
        self.edit_dsp_suite(move |s| {
            s.maximizer.threshold_db = -needed_boost;
            s.maximizer.ceiling_db = target_ceiling_dbtp;
        });
        Ok((current_lufs, needed_boost))
    }

    /// Loudness-matched A/B: play the processed signal at the input's loudness
    pub fn set_ab_gain_match(&self, enabled: bool) {
        self.shared.ab_match.store(enabled, Ordering::Relaxed);
    }

    pub fn ab_gain_match(&self) -> bool {
        self.shared.ab_match.load(Ordering::Relaxed)
    }

    fn update_trim_fade(&self, f: impl FnOnce(&mut TrimFadeSettings)) {
        let mut ui = self.ui.lock();
        f(&mut ui.trim_fade);
        let t = ui.trim_fade;
        ui.send(Command::SetTrimFade(t));
    }

    pub fn set_trim_fade(&self, settings: TrimFadeSettings) {
        self.update_trim_fade(|t| *t = settings);
    }

    pub fn trim_fade(&self) -> TrimFadeSettings {
        self.ui.lock().trim_fade
    }

    pub fn set_trim_start_fraction(&self, frac: f64) {
        self.update_trim_fade(|t| t.trim_start_fraction = frac.clamp(0.0, t.trim_end_fraction));
    }

    pub fn set_trim_end_fraction(&self, frac: f64) {
        self.update_trim_fade(|t| t.trim_end_fraction = frac.clamp(t.trim_start_fraction, 1.0));
    }

    pub fn set_fade_in_seconds(&self, sec: f64) {
        self.update_trim_fade(|t| t.fade_in_seconds = sec.max(0.0));
    }

    pub fn set_fade_out_seconds(&self, sec: f64) {
        self.update_trim_fade(|t| t.fade_out_seconds = sec.max(0.0));
    }

    pub fn with_montage<R>(&self, f: impl FnOnce(&mut AlbumMontage) -> R) -> R {
        f(&mut self.ui.lock().montage)
    }

    /// Enter or leave album montage mode. Entering parks the single track; call
    /// `refresh_montage_playback` afterwards to load the album sequence for playback.
    /// Leaving restores the parked track.
    pub fn set_montage_mode(&self, enabled: bool) {
        let mut ui = self.ui.lock();
        if ui.in_montage_mode == enabled {
            return;
        }
        ui.in_montage_mode = enabled;
        let next = if enabled {
            ui.single_track_buffer = ui.audio_buffer.take();
            None
        } else {
            ui.single_track_buffer.take()
        };
        ui.audio_buffer = next.clone();
        self.shared.cursor.store(0, Ordering::Relaxed);
        ui.send(Command::SetBuffer(next));
        Self::clear_ml_stems(&mut ui);
        self.clear_codec_preview(&mut ui);
        ui.send(Command::ResetProcessing);
    }

    /// Decode and sequence the montage tracks into the playback buffer. Run this off the UI
    /// thread; decoding happens without holding any engine lock.
    pub fn refresh_montage_playback(&self) -> Result<(), String> {
        let (montage, generation) = {
            let ui = self.ui.lock();
            if !ui.in_montage_mode {
                return Err("Montage mode is not active".to_string());
            }
            let generation = self.montage_generation.fetch_add(1, Ordering::SeqCst) + 1;
            (ui.montage.clone(), generation)
        };
        if montage.tracks.iter().all(|t| !t.is_active) {
            if self.in_montage_mode() {
                self.replace_audio_buffer_keep_position(None);
            }
            self.shared.cursor.store(0, Ordering::Relaxed);
            return Ok(());
        }
        let album = montage.build_album_buffer(|_, _| {})?;
        if !self.in_montage_mode() {
            return Err("Montage mode was closed during the rebuild".to_string());
        }
        if self.montage_generation.load(Ordering::SeqCst) != generation {
            return Ok(()); // a newer rebuild is in flight and will install its buffer
        }
        self.replace_audio_buffer_keep_position(Some(Arc::new(album)));
        Ok(())
    }

    pub fn in_montage_mode(&self) -> bool {
        self.ui.lock().in_montage_mode
    }

    pub fn set_processing_mode(&self, mode: ProcessingMode) {
        let mut ui = self.ui.lock();
        ui.mode = mode;
        ui.send(Command::SetMode(mode));
    }

    pub fn processing_mode(&self) -> ProcessingMode {
        self.ui.lock().mode
    }

    /// Install (or remove, with `None`) a fallback plugin, configured for the output device's rate.
    /// Device callbacks longer than `PLUGIN_MAX_BLOCK` are split by `Vst3Plugin::process_stereo`.
    pub fn set_vst3_plugin(&self, plugin: Option<Vst3Plugin>) -> Result<(), String> {
        let plugin = match plugin {
            Some(p) => {
                p.setup_audio(self.device_sample_rate() as f64, PLUGIN_MAX_BLOCK)?;
                Some(Arc::new(p))
            }
            None => None,
        };
        let mut ui = self.ui.lock();
        ui.plugin = plugin.clone();
        ui.send(Command::SetPlugin(plugin));
        Ok(())
    }

    /// Run `f` with the plugin taken off the audio thread, for long offline jobs. The audio thread
    /// confirms it has let go before `f` runs, and gets the plugin back afterwards unless another
    /// one was installed meanwhile. Playback passes audio through dry in between.
    pub fn with_detached_vst3_plugin<R>(&self, f: impl FnOnce(Option<&Vst3Plugin>) -> R) -> R {
        let plugin = {
            let mut ui = self.ui.lock();
            let plugin = ui.plugin.clone();
            if plugin.is_some() {
                let seq = ui.send(Command::SetPlugin(None));
                drop(ui);
                self.wait_applied(seq);
            }
            plugin
        };
        let result = f(plugin.as_deref());
        if let Some(p) = plugin {
            let mut ui = self.ui.lock();
            if ui.plugin.as_ref().map(|cur| Arc::ptr_eq(cur, &p)).unwrap_or(false) {
                let _ = p.setup_audio(self.device_sample_rate() as f64, PLUGIN_MAX_BLOCK);
                ui.send(Command::SetPlugin(Some(p)));
            }
        }
        result
    }

    /// UI-thread access to the plugin (editor, wheel, state). Processing continues meanwhile,
    /// per the VST3 threading model.
    pub fn with_vst3_plugin<R>(&self, f: impl FnOnce(Option<&Vst3Plugin>) -> R) -> R {
        let plugin = self.ui.lock().plugin.clone();
        f(plugin.as_deref())
    }

    /// Read the suite as the UI sees it, with live meters from the audio thread copied in
    pub fn with_dsp_suite<R>(&self, f: impl FnOnce(&MasteringSuite) -> R) -> R {
        let snap = self.shared.snapshot.lock().clone();
        let mut ui = self.ui.lock();
        for &kind in ModuleKind::ALL {
            ui.suite.module_mut(kind).set_meters(snap.module_meters[kind.index()]);
            ui.suite.show_auto_gain_db(kind, snap.auto_gain_db[kind.index()]);
        }
        // Mirror whether trained stems are standing in for the spectral stem modules
        let stems_current = match (&ui.ml_stems, &ui.audio_buffer) {
            (Some((src, _)), Some(buf)) => Arc::ptr_eq(src, buf),
            _ => false,
        };
        ui.suite.external_stems = stems_current && ui.mode == ProcessingMode::DspSuite && ui.suite.uses_stems();
        f(&ui.suite)
    }

    /// Change the suite: applied to the UI copy now and to the audio thread's copy at the start of
    /// its next block. `edit` must be deterministic so both copies end up identical.
    pub fn edit_dsp_suite(&self, edit: impl Fn(&mut MasteringSuite) + Send + Sync + 'static) {
        let edit: SuiteEdit = Arc::new(edit);
        let mut ui = self.ui.lock();
        edit(&mut ui.suite);
        ui.send(Command::EditSuite(edit));
    }

    /// Snapshot of the suite's settings, for offline renders
    pub fn dsp_suite_snapshot(&self) -> MasteringSuite {
        self.ui.lock().suite.clone()
    }

    /// Run Master Assistant analysis on the loaded track (slow: run it off the UI thread). With
    /// `use_reference`, a loaded reference track is the target instead of the genre profile.
    pub fn analyze_for_assistant(&self, target_lufs: f32, ceiling_dbtp: f32, genre: usize, use_reference: bool) -> Result<MasterAssistantReport, String> {
        let buf = self.audio_buffer().ok_or("No audio track loaded to analyze")?;
        let reference = if use_reference { self.reference_buffer() } else { None };
        MasterAssistant::analyze_with(&buf, &crate::dsp::AssistantOptions { target_lufs, ceiling_dbtp, genre, reference: reference.as_deref() })
    }

    /// Set the suite up as a Master Assistant report suggests
    pub fn apply_assistant(&self, report: &MasterAssistantReport) {
        let applied = report.clone();
        self.edit_dsp_suite(move |s| MasterAssistant::apply_to_suite(&applied, s));
    }

    /// Learn Match EQ from the loaded reference track (target) and main track (source), then
    /// switch Match EQ on
    pub fn learn_match_eq(&self) -> Result<(), String> {
        let source = self.audio_buffer().ok_or("Load a track first")?;
        let reference = self.reference_buffer().ok_or("Load a reference track first (+ Ref Track)")?;
        let mut learned = MatchEq::new(self.device_sample_rate() as f32);
        learned.learn(&source, &reference)?;
        let (src_db, ref_db) = learned.learned_spectra();
        let (src_db, ref_db) = (src_db.to_vec(), ref_db.to_vec());
        self.edit_dsp_suite(move |s| {
            s.match_eq.set_learned_spectra(src_db.clone(), ref_db.clone());
            crate::dsp::Module::set_enabled(&mut s.match_eq, true);
        });
        Ok(())
    }

    /// Load and apply an XML preset (this app's own, or the element/param factory layout)
    pub fn load_xml_preset<P: AsRef<Path>>(&self, path: P) -> Result<XmlPreset, String> {
        let preset = XmlPreset::from_file(path)?;
        self.apply_preset(&preset);
        Ok(preset)
    }

    /// Every suite setting as a preset (for undo, snapshots and sessions)
    pub fn suite_preset(&self) -> XmlPreset {
        XmlPreset::from_suite(&self.ui.lock().suite, "state")
    }

    /// Apply a preset to the whole suite. The configured suite is built here and swapped in on
    /// the audio thread, so nothing is parsed or allocated there.
    pub fn apply_preset(&self, preset: &XmlPreset) {
        let mut ui = self.ui.lock();
        let mut next = ui.suite.clone();
        preset.apply_to_suite(&mut next);
        next.reset_state();
        ui.suite = next.clone();
        ui.send(Command::ReplaceSuite(Box::new(next)));
    }

    /// Access and configure the 6-channel Stem Mixer
    pub fn with_stem_mixer<R>(&self, f: impl FnOnce(&mut StemMixer) -> R) -> R {
        f(&mut self.ui.lock().stem_mixer)
    }

    /// Sum all active stems into the master playback buffer, keeping the playhead where it is
    pub fn update_master_from_stems(&self) -> Result<(), String> {
        let master_buf = {
            let ui = self.ui.lock();
            if !ui.stem_mixer.has_loaded_stems() {
                return Err("No stems loaded".to_string());
            }
            ui.stem_mixer.sum_to_master_buffer()?
        };
        self.replace_audio_buffer_keep_position(Some(Arc::new(master_buf)));
        Ok(())
    }

    /// Automatically separate the loaded track into 6 stems
    pub fn separate_active_track_into_stems<F>(&self, progress_cb: F) -> Result<(), String>
    where
        F: Fn(f32, &str) + Send + Sync + 'static,
    {
        let buf = self.audio_buffer().ok_or_else(|| "No master audio track loaded to separate".to_string())?;
        if ml_separator::find_model().is_some() {
            // Trained model: drums, bass, other, vocals
            let stems = self.separate_with_model(|p| progress_cb(p, "Separating with the trained model..."))?;
            {
                let mut ui = self.ui.lock();
                let sm = &mut ui.stem_mixer;
                sm.clear_all();
                let [drums, bass, other, vocals] = [0, 1, 2, 3].map(|i| stems.stems[i].clone());
                sm.stems[0].load_buffer(drums, "Drums & Percussion");
                sm.stems[1].load_buffer(bass, "Bass");
                sm.stems[2].load_buffer(vocals, "Vocals");
                sm.stems[3].load_buffer(other, "Other instruments");
                sm.is_enabled = true;
            }
            return self.update_master_from_stems();
        }
        let result = StemSeparator::separate_stems(&buf, progress_cb)?;
        {
            let mut ui = self.ui.lock();
            let sm = &mut ui.stem_mixer;
            sm.stems[0].load_buffer(result.drums, "Drums & Percussion");
            sm.stems[1].load_buffer(result.bass, "Bass & Sub");
            sm.stems[2].load_buffer(result.vocals, "Vocals");
            sm.stems[3].load_buffer(result.instruments, "Guitars & Keys");
            sm.stems[4].load_buffer(result.synths, "Synths & FX");
            sm.stems[5].load_buffer(result.ambience, "Ambience & Reverb");
            sm.is_enabled = true;
        }
        self.update_master_from_stems()
    }

    pub fn get_meter_levels(&self) -> MeterLevels {
        let pl = self.shared.peak_l.load();
        let pr = self.shared.peak_r.load();
        let rl = self.shared.rms_l.load();
        let rr = self.shared.rms_r.load();
        let to_db = |val: f32| -> f32 {
            if val <= 1e-4 { -60.0 } else { (20.0 * val.log10()).clamp(-60.0, 6.0) }
        };
        let snap = self.shared.snapshot.lock().clone();
        MeterLevels {
            peak_l: pl,
            peak_r: pr,
            rms_l: rl,
            rms_r: rr,
            peak_l_db: to_db(pl),
            peak_r_db: to_db(pr),
            rms_l_db: to_db(rl),
            rms_r_db: to_db(rr),
            momentary_lufs: snap.momentary_lufs,
            short_term_lufs: snap.short_term_lufs,
            integrated_lufs: snap.integrated_lufs,
            true_peak_l_dbtp: snap.true_peak_l_dbtp,
            true_peak_r_dbtp: snap.true_peak_r_dbtp,
            phase_correlation: snap.phase_correlation,
            spectrum_db: snap.spectrum_db,
            ab_match_db: snap.ab_match_db,
        }
    }

    /// The output in use and the other devices on its host
    pub fn get_device_info(&self) -> AudioDeviceInfo {
        let d = self.device.lock().clone();
        let available_devices = list_output_devices(Some(&d.host)).into_iter().map(|o| o.name).collect();
        let frames = d.buffer_frames.unwrap_or(1024);
        let latency_ms = if d.sample_rate > 0 { frames as f32 / d.sample_rate as f32 * 1000.0 } else { 0.0 };
        AudioDeviceInfo {
            name: d.name,
            host: d.host,
            sample_rate: d.sample_rate,
            channels: d.channels,
            sample_format: d.sample_format,
            buffer_frames: d.buffer_frames,
            latency_ms,
            available_devices,
        }
    }

}
