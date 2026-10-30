use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use parking_lot::Mutex;

use super::buffer::AudioBuffer;
use super::montage::AlbumMontage;
use super::separator::StemSeparator;
use super::stems::StemMixer;
use super::trim::TrimFadeSettings;
use crate::dsp::{MasterAssistant, MasterAssistantReport, MasteringSuite, MatchEq, Module, XmlPreset};
use crate::vst3::Vst3Plugin;
use std::path::Path;

const FP_ONE: f64 = 4294967296.0; // 2^32 for 32.32 fixed-point position

/// Largest block handed to a VST3 plugin in one `process` call; longer device callbacks are split
const PLUGIN_MAX_BLOCK: usize = 2048;

/// Per-callback working buffers, allocated once and grown only if the device asks for more frames
#[derive(Default)]
struct CallbackScratch {
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    device_f32: Vec<f32>,
}

impl CallbackScratch {
    fn prepare(&mut self, num_frames: usize) {
        for buf in [&mut self.in_l, &mut self.in_r, &mut self.out_l, &mut self.out_r] {
            if buf.len() < num_frames {
                buf.resize(num_frames, 0.0);
            }
            buf[..num_frames].fill(0.0);
        }
    }
}

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
        }
    }
}

pub struct EngineState {
    pub audio_buffer: Option<AudioBuffer>,
    pub reference_buffer: Option<AudioBuffer>,
    pub reference_gain_offset: f32,
    pub vst3_plugin: Option<Vst3Plugin>,
    pub dsp_suite: MasteringSuite,
    pub mode: ProcessingMode,
    pub trim_fade: TrimFadeSettings,
    pub montage: AlbumMontage,
    pub in_montage_mode: bool,
    /// Single-track buffer parked while the album montage is loaded for playback
    pub single_track_buffer: Option<AudioBuffer>,
    pub stem_mixer: StemMixer,
}

impl EngineState {
    /// Buffer the transport is currently playing (reference track while auditioning it)
    fn active_buffer(&self, referencing: bool) -> Option<&AudioBuffer> {
        if referencing && self.reference_buffer.is_some() {
            self.reference_buffer.as_ref()
        } else {
            self.audio_buffer.as_ref()
        }
    }
}

pub struct AudioEngine {
    _stream: Stream,
    state: Arc<Mutex<EngineState>>,
    play_cursor_fixed: Arc<AtomicU64>,
    is_playing: Arc<AtomicBool>,
    is_looping: Arc<AtomicBool>,
    is_referencing: Arc<AtomicBool>,
    device_name: String,
    device_sample_rate: u32,
    device_channels: usize,
    device_sample_format: String,
    meter_peak_l: Arc<Mutex<f32>>,
    meter_peak_r: Arc<Mutex<f32>>,
    meter_rms_l: Arc<Mutex<f32>>,
    meter_rms_r: Arc<Mutex<f32>>,
    /// Bumped on every montage rebuild so a slower, older rebuild can't overwrite a newer one
    montage_generation: AtomicU64,
}

#[derive(Clone, Debug)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub host: String,
    pub sample_rate: u32,
    pub channels: usize,
    pub sample_format: String,
    pub latency_ms: f32,
    pub available_devices: Vec<String>,
}

unsafe impl Send for AudioEngine {}
unsafe impl Sync for AudioEngine {}

impl AudioEngine {
    pub fn new() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "No default audio output device available".to_string())?;

        let supported_config = device
            .default_output_config()
            .map_err(|e| format!("Failed to query default audio output config: {}", e))?;

        let sample_format = supported_config.sample_format();
        let device_sample_format = format!("{:?}", sample_format);
        let device_name = device.name().unwrap_or_else(|_| "WASAPI Default Endpoint".to_string());
        let config: StreamConfig = supported_config.into();
        let device_sample_rate = config.sample_rate.0;
        let channels = config.channels as usize;

        let state = Arc::new(Mutex::new(EngineState {
            audio_buffer: None,
            reference_buffer: None,
            reference_gain_offset: 1.0,
            vst3_plugin: None,
            dsp_suite: MasteringSuite::new(device_sample_rate as f32),
            mode: ProcessingMode::DspSuite,
            trim_fade: TrimFadeSettings::default(),
            montage: AlbumMontage::default(),
            in_montage_mode: false,
            single_track_buffer: None,
            stem_mixer: StemMixer::default(),
        }));

        let play_cursor_fixed = Arc::new(AtomicU64::new(0));
        let is_playing = Arc::new(AtomicBool::new(false));
        let is_looping = Arc::new(AtomicBool::new(false));
        let is_referencing = Arc::new(AtomicBool::new(false));

        let meter_peak_l = Arc::new(Mutex::new(0.0f32));
        let meter_peak_r = Arc::new(Mutex::new(0.0f32));
        let meter_rms_l = Arc::new(Mutex::new(0.0f32));
        let meter_rms_r = Arc::new(Mutex::new(0.0f32));

        let state_clone = Arc::clone(&state);
        let cursor_clone = Arc::clone(&play_cursor_fixed);
        let playing_clone = Arc::clone(&is_playing);
        let looping_clone = Arc::clone(&is_looping);
        let referencing_clone = Arc::clone(&is_referencing);
        let peak_l_clone = Arc::clone(&meter_peak_l);
        let peak_r_clone = Arc::clone(&meter_peak_r);
        let rms_l_clone = Arc::clone(&meter_rms_l);
        let rms_r_clone = Arc::clone(&meter_rms_r);

        let err_fn = |err| eprintln!("Audio stream error: {}", err);

        let ref_clone_16 = Arc::clone(&referencing_clone);
        let mut scratch_f32 = CallbackScratch::default();
        let mut scratch_i16 = CallbackScratch::default();
        let stream = match sample_format {
            SampleFormat::F32 => device
                .build_output_stream(
                    &config,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        Self::process_audio_f32(
                            data,
                            &mut scratch_f32,
                            channels,
                            device_sample_rate,
                            &state_clone,
                            &cursor_clone,
                            &playing_clone,
                            &looping_clone,
                            &referencing_clone,
                            &peak_l_clone,
                            &peak_r_clone,
                            &rms_l_clone,
                            &rms_r_clone,
                        );
                    },
                    err_fn,
                    None,
                )
                .map_err(|e| format!("Failed to build F32 output stream: {}", e))?,
            SampleFormat::I16 => device
                .build_output_stream(
                    &config,
                    move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                        let mut f32_buf = std::mem::take(&mut scratch_i16.device_f32);
                        if f32_buf.len() < data.len() {
                            f32_buf.resize(data.len(), 0.0);
                        }
                        Self::process_audio_f32(
                            &mut f32_buf[..data.len()],
                            &mut scratch_i16,
                            channels,
                            device_sample_rate,
                            &state_clone,
                            &cursor_clone,
                            &playing_clone,
                            &looping_clone,
                            &ref_clone_16,
                            &peak_l_clone,
                            &peak_r_clone,
                            &rms_l_clone,
                            &rms_r_clone,
                        );
                        for (out, &s) in data.iter_mut().zip(f32_buf.iter()) {
                            *out = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                        }
                        scratch_i16.device_f32 = f32_buf;
                    },
                    err_fn,
                    None,
                )
                .map_err(|e| format!("Failed to build I16 output stream: {}", e))?,
            _ => return Err(format!("Unsupported audio sample format: {:?}", sample_format)),
        };

        stream
            .play()
            .map_err(|e| format!("Failed to start audio stream: {}", e))?;

        Ok(Self {
            _stream: stream,
            state,
            play_cursor_fixed,
            is_playing,
            is_looping,
            is_referencing,
            device_name,
            device_sample_rate,
            device_channels: channels,
            device_sample_format,
            meter_peak_l,
            meter_peak_r,
            meter_rms_l,
            meter_rms_r,
            montage_generation: AtomicU64::new(0),
        })
    }

    fn process_audio_f32(
        output: &mut [f32],
        scratch: &mut CallbackScratch,
        num_device_channels: usize,
        device_sr: u32,
        state_arc: &Arc<Mutex<EngineState>>,
        cursor_atomic: &Arc<AtomicU64>,
        playing_atomic: &Arc<AtomicBool>,
        looping_atomic: &Arc<AtomicBool>,
        referencing_atomic: &Arc<AtomicBool>,
        meter_peak_l: &Arc<Mutex<f32>>,
        meter_peak_r: &Arc<Mutex<f32>>,
        meter_rms_l: &Arc<Mutex<f32>>,
        meter_rms_r: &Arc<Mutex<f32>>,
    ) {
        let num_frames = output.len() / num_device_channels;
        if num_frames == 0 {
            return;
        }

        output.fill(0.0);

        if !playing_atomic.load(Ordering::Relaxed) {
            // Decay meters smoothly when stopped
            let mut pl = meter_peak_l.lock();
            let mut pr = meter_peak_r.lock();
            let mut rl = meter_rms_l.lock();
            let mut rr = meter_rms_r.lock();
            *pl *= 0.85;
            *pr *= 0.85;
            *rl *= 0.85;
            *rr *= 0.85;
            return;
        }

        scratch.prepare(num_frames);
        let in_l = &mut scratch.in_l[..num_frames];
        let in_r = &mut scratch.in_r[..num_frames];
        let out_l = &mut scratch.out_l[..num_frames];
        let out_r = &mut scratch.out_r[..num_frames];

        let mut reached_end = false;

        {
            let mut state = match state_arc.try_lock() {
                Some(guard) => guard,
                None => return, // Skip block if state is being safely swapped by UI
            };

            if referencing_atomic.load(Ordering::Relaxed) && state.reference_buffer.is_some() {
                if let Some(ref ref_buf) = state.reference_buffer {
                    let total_frames = ref_buf.num_frames();
                    if total_frames > 0 {
                        let mut cur_fixed = cursor_atomic.load(Ordering::Relaxed);
                        let looping = looping_atomic.load(Ordering::Relaxed);
                        let step = ((ref_buf.sample_rate as f64 / device_sr as f64) * FP_ONE) as u64;
                        let gain_offset = state.reference_gain_offset;

                        for i in 0..num_frames {
                            let cur_frame_idx = (cur_fixed >> 32) as usize;
                            if cur_frame_idx >= total_frames {
                                if looping {
                                    cur_fixed = 0;
                                } else {
                                    reached_end = true;
                                    break;
                                }
                            }
                            let cur_pos_f = (cur_fixed as f64) / FP_ONE;
                            let (s_l, s_r) = ref_buf.get_interpolated_frame(cur_pos_f);
                            out_l[i] = s_l * gain_offset;
                            out_r[i] = s_r * gain_offset;
                            cur_fixed += step;
                        }
                        cursor_atomic.store(cur_fixed, Ordering::Relaxed);
                    }
                }
                state.dsp_suite.lufs_meter.process_block(out_l, out_r);
                state.dsp_suite.spectral_analyzer.feed_samples(out_l, out_r);
            } else {
                if let Some(ref buffer) = state.audio_buffer {
                    let total_frames = buffer.num_frames();
                    if total_frames > 0 {
                        let mut cur_fixed = cursor_atomic.load(Ordering::Relaxed);
                        let looping = looping_atomic.load(Ordering::Relaxed);
                        let (start_frame, end_frame) = state.trim_fade.get_frame_boundaries(total_frames);

                        let cur_idx = (cur_fixed >> 32) as usize;
                        if cur_idx < start_frame || cur_idx >= end_frame {
                            cur_fixed = (start_frame as u64) << 32;
                        }

                        // Fractional step per device output frame for pitch-perfect playback
                        let step = ((buffer.sample_rate as f64 / device_sr as f64) * FP_ONE) as u64;

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

                            let cur_pos_f = (cur_fixed as f64) / FP_ONE;
                            let (s_l, s_r) = buffer.get_interpolated_frame(cur_pos_f);
                            let fade_gain = state.trim_fade.calculate_gain(cur_frame_idx, start_frame, end_frame, buffer.sample_rate);
                            in_l[i] = s_l * fade_gain;
                            in_r[i] = s_r * fade_gain;
                            cur_fixed += step;
                        }
                        cursor_atomic.store(cur_fixed, Ordering::Relaxed);
                    }
                }

                match state.mode {
                    ProcessingMode::Vst3 => {
                        if let Some(ref mut plugin) = state.vst3_plugin {
                            plugin.process_stereo(in_l, in_r, out_l, out_r);
                        } else {
                            out_l.copy_from_slice(in_l);
                            out_r.copy_from_slice(in_r);
                        }
                        state.dsp_suite.lufs_meter.process_block(out_l, out_r);
                        state.dsp_suite.spectral_analyzer.feed_samples(out_l, out_r);
                    }
                    ProcessingMode::DspSuite => {
                        out_l.copy_from_slice(in_l);
                        out_r.copy_from_slice(in_r);
                        state.dsp_suite.process_stereo(out_l, out_r);
                    }
                    ProcessingMode::Bypass => {
                        out_l.copy_from_slice(in_l);
                        out_r.copy_from_slice(in_r);
                        state.dsp_suite.lufs_meter.process_block(out_l, out_r);
                        state.dsp_suite.spectral_analyzer.feed_samples(out_l, out_r);
                    }
                }
            }
        }

        if reached_end {
            playing_atomic.store(false, Ordering::Relaxed);
        }

        // Calculate Peak and RMS for metering
        let mut max_l = 0.0f32;
        let mut max_r = 0.0f32;
        let mut sum_sq_l = 0.0f32;
        let mut sum_sq_r = 0.0f32;

        for i in 0..num_frames {
            let sl = out_l[i];
            let sr = out_r[i];

            let abs_l = sl.abs();
            let abs_r = sr.abs();
            if abs_l > max_l { max_l = abs_l; }
            if abs_r > max_r { max_r = abs_r; }
            sum_sq_l += sl * sl;
            sum_sq_r += sr * sr;

            // Interleave into device output
            output[i * num_device_channels] = sl;
            if num_device_channels > 1 {
                output[i * num_device_channels + 1] = sr;
            }
        }

        let rms_l = (sum_sq_l / num_frames as f32).sqrt();
        let rms_r = (sum_sq_r / num_frames as f32).sqrt();

        // Update meters with smooth decay
        {
            let mut pl = meter_peak_l.lock();
            let mut pr = meter_peak_r.lock();
            let mut rl = meter_rms_l.lock();
            let mut rr = meter_rms_r.lock();

            *pl = pl.max(max_l) * 0.92;
            *pr = pr.max(max_r) * 0.92;
            *rl = rl.max(rms_l) * 0.92;
            *rr = rr.max(rms_r) * 0.92;
        }
    }

    pub fn device_sample_rate(&self) -> u32 {
        self.device_sample_rate
    }

    pub fn play(&self) {
        self.is_playing.store(true, Ordering::Relaxed);
    }

    pub fn pause(&self) {
        self.is_playing.store(false, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.is_playing.store(false, Ordering::Relaxed);
        self.play_cursor_fixed.store(0, Ordering::Relaxed);
        let mut state = self.state.lock();
        Self::reset_processing(&mut state);
    }

    /// Clear filter/limiter memory and loudness history so the next pass starts clean
    fn reset_processing(state: &mut EngineState) {
        state.dsp_suite.reset_state();
        if let Some(ref mut plugin) = state.vst3_plugin {
            plugin.seek(0);
        }
    }

    pub fn is_playing(&self) -> bool {
        self.is_playing.load(Ordering::Relaxed)
    }

    pub fn set_loop(&self, enabled: bool) {
        self.is_looping.store(enabled, Ordering::Relaxed);
    }

    pub fn is_looping(&self) -> bool {
        self.is_looping.load(Ordering::Relaxed)
    }

    pub fn seek_frame(&self, frame: usize) {
        let fixed = (frame as u64) << 32;
        self.play_cursor_fixed.store(fixed, Ordering::Relaxed);
    }

    pub fn seek_fraction(&self, fraction: f64) {
        let state = self.state.lock();
        if let Some(buf) = state.active_buffer(self.is_referencing()) {
            let target_frame = (buf.num_frames() as f64 * fraction.clamp(0.0, 1.0)) as usize;
            let fixed = (target_frame as u64) << 32;
            self.play_cursor_fixed.store(fixed, Ordering::Relaxed);
        }
    }

    pub fn current_frame(&self) -> usize {
        (self.play_cursor_fixed.load(Ordering::Relaxed) >> 32) as usize
    }

    pub fn current_position_seconds(&self) -> f64 {
        let state = self.state.lock();
        if let Some(buf) = state.active_buffer(self.is_referencing()) {
            if buf.sample_rate > 0 {
                let frame_f = (self.play_cursor_fixed.load(Ordering::Relaxed) as f64) / FP_ONE;
                return frame_f / buf.sample_rate as f64;
            }
        }
        0.0
    }

    pub fn duration_seconds(&self) -> f64 {
        let state = self.state.lock();
        if let Some(buf) = state.active_buffer(self.is_referencing()) {
            return buf.duration_seconds();
        }
        0.0
    }

    /// Load a new track: rewinds the transport and clears processing state from the previous track.
    /// The DSP suite stays at the device rate because playback is resampled before processing.
    pub fn set_audio_buffer(&self, buffer: AudioBuffer) {
        let mut state = self.state.lock();
        if state.in_montage_mode {
            // A track opened while the montage is loaded replaces the parked single track
            state.in_montage_mode = false;
            state.single_track_buffer = None;
        }
        state.audio_buffer = Some(buffer);
        Self::reset_processing(&mut state);
        self.play_cursor_fixed.store(0, Ordering::Relaxed);
    }

    /// Swap the playing buffer without rewinding (stem remixes, montage rebuilds)
    fn replace_audio_buffer_keep_position(&self, buffer: AudioBuffer) {
        let mut state = self.state.lock();
        let max_fixed = (buffer.num_frames() as u64) << 32;
        if self.play_cursor_fixed.load(Ordering::Relaxed) > max_fixed {
            self.play_cursor_fixed.store(0, Ordering::Relaxed);
        }
        state.audio_buffer = Some(buffer);
    }

    pub fn set_reference_buffer(&self, buffer: AudioBuffer) {
        let mut state = self.state.lock();
        state.reference_buffer = Some(buffer);
    }

    pub fn with_reference_buffer<R>(&self, f: impl FnOnce(&Option<AudioBuffer>) -> R) -> R {
        let state = self.state.lock();
        f(&state.reference_buffer)
    }

    pub fn set_referencing(&self, enabled: bool) {
        self.is_referencing.store(enabled, Ordering::Relaxed);
    }

    pub fn is_referencing(&self) -> bool {
        self.is_referencing.load(Ordering::Relaxed)
    }

    pub fn set_reference_gain_offset(&self, offset: f32) {
        let mut state = self.state.lock();
        state.reference_gain_offset = offset.clamp(0.01, 10.0);
    }

    pub fn reference_gain_offset(&self) -> f32 {
        let state = self.state.lock();
        state.reference_gain_offset
    }

    /// Auto-matches reference track loudness to main track via delta-LUFS gain offset
    pub fn auto_match_reference_gain(&self) -> Result<(f32, f32, f32), String> {
        let state = self.state.lock();
        let main_buf = state.audio_buffer.as_ref().ok_or("No main track loaded to match against")?;
        let ref_buf = state.reference_buffer.as_ref().ok_or("No reference track loaded")?;

        let main_lufs = crate::dsp::lufs::calculate_buffer_integrated_lufs(main_buf);
        let ref_lufs = crate::dsp::lufs::calculate_buffer_integrated_lufs(ref_buf);

        if main_lufs <= -69.0 || ref_lufs <= -69.0 {
            return Err("Signal level too low to accurately compute LUFS".to_string());
        }

        let delta_db = main_lufs - ref_lufs;
        let gain_offset = 10.0f32.powf(delta_db / 20.0);
        drop(state);

        self.set_reference_gain_offset(gain_offset);
        Ok((main_lufs, ref_lufs, delta_db))
    }

    /// Match Maximizer parameters to target streaming standard
    pub fn apply_loudness_target(&self, target_lufs: f32, target_ceiling_dbtp: f32) -> Result<(f32, f32), String> {
        let mut state = self.state.lock();
        let main_buf = state.audio_buffer.as_ref().ok_or("No main track loaded")?;
        let current_lufs = crate::dsp::lufs::calculate_buffer_integrated_lufs(main_buf);

        if current_lufs <= -69.0 {
            return Err("Signal level too low to compute target drive".to_string());
        }

        let needed_boost = (target_lufs - current_lufs).max(0.0);
        // Apply drive to maximizer threshold (negative threshold = drive)
        state.dsp_suite.maximizer.threshold_db = -needed_boost;
        state.dsp_suite.maximizer.ceiling_db = target_ceiling_dbtp;

        Ok((current_lufs, needed_boost))
    }

    pub fn set_trim_fade(&self, settings: TrimFadeSettings) {
        let mut state = self.state.lock();
        state.trim_fade = settings;
    }

    pub fn trim_fade(&self) -> TrimFadeSettings {
        let state = self.state.lock();
        state.trim_fade
    }

    pub fn set_trim_start_fraction(&self, frac: f64) {
        let mut state = self.state.lock();
        state.trim_fade.trim_start_fraction = frac.clamp(0.0, state.trim_fade.trim_end_fraction);
    }

    pub fn set_trim_end_fraction(&self, frac: f64) {
        let mut state = self.state.lock();
        state.trim_fade.trim_end_fraction = frac.clamp(state.trim_fade.trim_start_fraction, 1.0);
    }

    pub fn set_fade_in_seconds(&self, sec: f64) {
        let mut state = self.state.lock();
        state.trim_fade.fade_in_seconds = sec.max(0.0);
    }

    pub fn set_fade_out_seconds(&self, sec: f64) {
        let mut state = self.state.lock();
        state.trim_fade.fade_out_seconds = sec.max(0.0);
    }

    pub fn with_montage<R>(&self, f: impl FnOnce(&mut AlbumMontage) -> R) -> R {
        let mut state = self.state.lock();
        f(&mut state.montage)
    }

    /// Enter or leave album montage mode. Entering parks the single track; call
    /// `refresh_montage_playback` afterwards to load the album sequence for playback.
    /// Leaving restores the parked track.
    pub fn set_montage_mode(&self, enabled: bool) {
        let mut state = self.state.lock();
        if state.in_montage_mode == enabled {
            return;
        }
        state.in_montage_mode = enabled;
        if enabled {
            state.single_track_buffer = state.audio_buffer.take();
        } else {
            state.audio_buffer = state.single_track_buffer.take();
        }
        Self::reset_processing(&mut state);
        self.play_cursor_fixed.store(0, Ordering::Relaxed);
    }

    /// Decode and sequence the montage tracks into the playback buffer. Decoding happens
    /// without holding the engine lock, so run this off the UI thread.
    pub fn refresh_montage_playback(&self) -> Result<(), String> {
        let (montage, generation) = {
            let state = self.state.lock();
            if !state.in_montage_mode {
                return Err("Montage mode is not active".to_string());
            }
            let generation = self.montage_generation.fetch_add(1, Ordering::SeqCst) + 1;
            (state.montage.clone(), generation)
        };
        if montage.tracks.iter().all(|t| !t.is_active) {
            let mut state = self.state.lock();
            if state.in_montage_mode {
                state.audio_buffer = None;
            }
            self.play_cursor_fixed.store(0, Ordering::Relaxed);
            return Ok(());
        }
        let album = montage.build_album_buffer(|_, _| {})?;
        if !self.in_montage_mode() {
            return Err("Montage mode was closed during the rebuild".to_string());
        }
        if self.montage_generation.load(Ordering::SeqCst) != generation {
            return Ok(()); // a newer rebuild is in flight and will install its buffer
        }
        self.replace_audio_buffer_keep_position(album);
        Ok(())
    }

    pub fn in_montage_mode(&self) -> bool {
        let state = self.state.lock();
        state.in_montage_mode
    }

    pub fn set_processing_mode(&self, mode: ProcessingMode) {
        let mut state = self.state.lock();
        state.mode = mode;
    }

    pub fn processing_mode(&self) -> ProcessingMode {
        let state = self.state.lock();
        state.mode
    }

    /// Install a plugin, configured for the output device's rate. Device callbacks longer than
    /// `PLUGIN_MAX_BLOCK` are split by `Vst3Plugin::process_stereo`.
    pub fn set_vst3_plugin(&self, plugin: Option<Vst3Plugin>) -> Result<(), String> {
        let mut plugin = plugin;
        if let Some(ref mut p) = plugin {
            p.setup_audio(self.device_sample_rate as f64, PLUGIN_MAX_BLOCK)?;
        }
        let mut state = self.state.lock();
        state.vst3_plugin = plugin;
        Ok(())
    }

    /// Run `f` with the plugin removed from the engine so long jobs (offline renders) don't hold
    /// the engine lock. Playback passes audio through dry meanwhile. The plugin is put back
    /// afterwards unless another one was installed in the meantime.
    pub fn with_detached_vst3_plugin<R>(&self, f: impl FnOnce(&mut Option<Vst3Plugin>) -> R) -> R {
        let mut plugin = self.state.lock().vst3_plugin.take();
        let result = f(&mut plugin);
        let mut state = self.state.lock();
        if state.vst3_plugin.is_none() {
            state.vst3_plugin = plugin;
        }
        result
    }

    pub fn with_vst3_plugin<R>(&self, f: impl FnOnce(&mut Option<Vst3Plugin>) -> R) -> R {
        let mut state = self.state.lock();
        f(&mut state.vst3_plugin)
    }

    pub fn with_dsp_suite<R>(&self, f: impl FnOnce(&mut MasteringSuite) -> R) -> R {
        let mut state = self.state.lock();
        f(&mut state.dsp_suite)
    }

    pub fn with_audio_buffer<R>(&self, f: impl FnOnce(&Option<AudioBuffer>) -> R) -> R {
        let state = self.state.lock();
        f(&state.audio_buffer)
    }

    /// Run intelligent Master Assistant profiling on active audio buffer
    pub fn run_master_assistant(&self, target_lufs: f32) -> Result<MasterAssistantReport, String> {
        let buf = {
            let state = self.state.lock();
            match state.audio_buffer {
                Some(ref b) => b.clone(),
                None => return Err("No audio track loaded to analyze".to_string()),
            }
        };

        let report = MasterAssistant::analyze(&buf, target_lufs)?;
        {
            let mut state = self.state.lock();
            MasterAssistant::apply_to_suite(&report, &mut state.dsp_suite);
        }
        Ok(report)
    }

    /// Learn Match EQ from the loaded reference track (target) and main track (source), then
    /// switch Match EQ on. Analysis runs without holding the engine lock.
    pub fn learn_match_eq(&self) -> Result<(), String> {
        let (source, reference, sr) = {
            let state = self.state.lock();
            let src = state.audio_buffer.clone().ok_or("Load a track first")?;
            let refr = state.reference_buffer.clone().ok_or("Load a reference track first (+ Ref Track)")?;
            (src, refr, state.dsp_suite.sample_rate)
        };
        let mut learned = MatchEq::new(sr);
        learned.learn(&source, &reference)?;
        let (src_db, ref_db) = learned.learned_spectra();
        let mut state = self.state.lock();
        state.dsp_suite.match_eq.set_learned_spectra(src_db.to_vec(), ref_db.to_vec());
        state.dsp_suite.match_eq.set_enabled(true);
        Ok(())
    }

    /// Load and apply an XML preset (this app's own, or the element/param factory layout)
    pub fn load_xml_preset<P: AsRef<Path>>(&self, path: P) -> Result<XmlPreset, String> {
        let preset = XmlPreset::from_file(path)?;
        {
            let mut state = self.state.lock();
            preset.apply_to_suite(&mut state.dsp_suite);
        }
        Ok(preset)
    }

    /// Access and configure the 6-channel Stem Mixer
    pub fn with_stem_mixer<R>(&self, f: impl FnOnce(&mut StemMixer) -> R) -> R {
        let mut state = self.state.lock();
        f(&mut state.stem_mixer)
    }

    /// Sum all active stems into the master playback buffer, keeping the playhead where it is
    pub fn update_master_from_stems(&self) -> Result<(), String> {
        let master_buf = {
            let state = self.state.lock();
            if !state.stem_mixer.has_loaded_stems() {
                return Err("No stems loaded".to_string());
            }
            state.stem_mixer.sum_to_master_buffer()?
        };
        self.replace_audio_buffer_keep_position(master_buf);
        Ok(())
    }

    /// Automatically separate active master audio track into 6 high-fidelity stems
    pub fn separate_active_track_into_stems<F>(&self, progress_cb: F) -> Result<(), String>
    where
        F: Fn(f32, &str) + Send + Sync + 'static,
    {
        let buf = {
            let state = self.state.lock();
            state.audio_buffer.clone().ok_or_else(|| "No master audio track loaded to separate".to_string())?
        };

        let result = StemSeparator::separate_stems(&buf, progress_cb)?;

        {
            let mut state = self.state.lock();
            state.stem_mixer.stems[0].load_buffer(result.drums, "Drums & Percussion");
            state.stem_mixer.stems[1].load_buffer(result.bass, "Bass & Sub");
            state.stem_mixer.stems[2].load_buffer(result.vocals, "Vocals");
            state.stem_mixer.stems[3].load_buffer(result.instruments, "Guitars & Keys");
            state.stem_mixer.stems[4].load_buffer(result.synths, "Synths & FX");
            state.stem_mixer.stems[5].load_buffer(result.ambience, "Ambience & Reverb");
            state.stem_mixer.is_enabled = true;
        }

        self.update_master_from_stems()?;
        Ok(())
    }

    pub fn get_meter_levels(&self) -> MeterLevels {
        let pl = *self.meter_peak_l.lock();
        let pr = *self.meter_peak_r.lock();
        let rl = *self.meter_rms_l.lock();
        let rr = *self.meter_rms_r.lock();

        let to_db = |val: f32| -> f32 {
            if val <= 1e-4 {
                -60.0
            } else {
                (20.0 * val.log10()).clamp(-60.0, 6.0)
            }
        };

        let (momentary, short_term, integrated, tp_l, tp_r, phase_corr, spectrum) = {
            if let Some(mut state) = self.state.try_lock() {
                state.dsp_suite.spectral_analyzer.update_spectrum();
                let r = state.dsp_suite.lufs_meter.get_readout();
                let spec = *state.dsp_suite.spectral_analyzer.get_spectrum_db();
                (r.momentary_lufs, r.short_term_lufs, r.integrated_lufs, r.true_peak_l_dbtp, r.true_peak_r_dbtp, r.phase_correlation, spec)
            } else {
                (-70.0, -70.0, -70.0, -60.0, -60.0, 1.0, [-90.0; 128])
            }
        };

        MeterLevels {
            peak_l: pl,
            peak_r: pr,
            rms_l: rl,
            rms_r: rr,
            peak_l_db: to_db(pl),
            peak_r_db: to_db(pr),
            rms_l_db: to_db(rl),
            rms_r_db: to_db(rr),
            momentary_lufs: momentary,
            short_term_lufs: short_term,
            integrated_lufs: integrated,
            true_peak_l_dbtp: tp_l,
            true_peak_r_dbtp: tp_r,
            phase_correlation: phase_corr,
            spectrum_db: spectrum,
        }
    }

    /// Query WASAPI audio hardware details, device topology, and latency diagnostic metrics
    pub fn get_device_info(&self) -> AudioDeviceInfo {
        let host = cpal::default_host();
        let available_devices = host.output_devices().map(|devices| {
            devices.filter_map(|d| d.name().ok()).collect()
        }).unwrap_or_default();

        let latency_ms = if self.device_sample_rate > 0 {
            (1024.0 / self.device_sample_rate as f32) * 1000.0
        } else {
            0.0
        };

        AudioDeviceInfo {
            name: self.device_name.clone(),
            host: "WASAPI (Windows Audio Session API Exclusive / Shared Low-Latency)".to_string(),
            sample_rate: self.device_sample_rate,
            channels: self.device_channels,
            sample_format: self.device_sample_format.clone(),
            latency_ms,
            available_devices,
        }
    }
}
