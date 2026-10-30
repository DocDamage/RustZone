//! Application state, toolbar, transport, shortcuts, MIDI, background jobs and dialogs

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use eframe::egui::{self, Color32, RichText, Vec2};
use parking_lot::Mutex;

use super::plugin_window::PluginWindow;
use super::widgets::{self, WaveformAction, ACCENT, DIM, WARN};
use super::{discover_vst3_plugins, save_png, LaunchOptions, AUDIO_EXTENSIONS, EXPORT_FORMATS, TARGETS};
use crate::audio::{
    list_audio_hosts, list_output_devices, load_audio_file, render_to_wav, AudioEngine, BwfMetadata, DeviceChoice,
    FadeShape, OutputDeviceInfo, ProcessingMode, RenderOptions, RenderSource,
};
use crate::dsp::{calculate_buffer_integrated_lufs, MasteringSuite, ModuleKind, XmlPreset};
use crate::midi::{MidiController, MidiEvent};
use crate::session::{History, Session, SessionUiState, Snapshots, SESSION_EXTENSION, SLOT_NAMES};
use crate::vst3::Vst3Plugin;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Suite,
    Album,
    Stems,
}

/// A processor choice in the toolbar
#[derive(Clone)]
pub(super) enum Processor {
    BuiltIn,
    Vst3(String, PathBuf),
}

/// Something a MIDI controller can drive
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum MidiTarget {
    Param(ModuleKind, usize),
}

/// Thread-safe status line that worker threads write to
#[derive(Clone, Default)]
pub(super) struct Status(Arc<Mutex<String>>);

impl Status {
    pub fn set(&self, text: impl Into<String>) {
        *self.0.lock() = text.into();
    }
    pub fn get(&self) -> String {
        self.0.lock().clone()
    }
}

/// Device dialog state
pub(super) struct DeviceDialog {
    open: bool,
    hosts: Vec<String>,
    devices: Vec<OutputDeviceInfo>,
    choice: DeviceChoice,
}

pub struct MasteringApp {
    pub(super) engine: Arc<AudioEngine>,
    pub(super) history: History,
    pub(super) snapshots: Snapshots,
    pub(super) view: View,
    pub(super) selected_module: ModuleKind,
    pub(super) selected_band: Option<usize>,
    pub(super) export_format: usize,
    pub(super) target: usize,
    pub(super) status: Status,
    pub(super) busy: Arc<AtomicBool>,
    pub(super) ctx: egui::Context,
    track_path: Option<PathBuf>,
    reference_path: Option<PathBuf>,
    processors: Vec<Processor>,
    processor: usize,
    pre_bypass_mode: ProcessingMode,
    plugin_window: Option<PluginWindow>,
    main_hwnd: Option<windows::Win32::Foundation::HWND>,
    _midi: Option<MidiController>,
    midi_rx: Option<Receiver<MidiEvent>>,
    pub(super) midi_map: HashMap<(u8, u8), MidiTarget>,
    pub(super) midi_learn: Option<MidiTarget>,
    device_dialog: DeviceDialog,
    assistant_report: Option<String>,
    screenshot: Option<(PathBuf, u32)>,
    started: Instant,
}

impl MasteringApp {
    pub fn new(cc: &eframe::CreationContext<'_>, options: LaunchOptions) -> Result<Self, String> {
        let ctx = cc.egui_ctx.clone();
        let mut visuals = egui::Visuals::dark();
        visuals.selection.bg_fill = Color32::from_rgb(0, 120, 150);
        visuals.panel_fill = Color32::from_rgb(22, 23, 27);
        ctx.set_visuals(visuals);

        let engine = Arc::new(AudioEngine::new()?);
        let mut processors = vec![Processor::BuiltIn];
        for p in discover_vst3_plugins() {
            let name = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
            processors.push(Processor::Vst3(name, p));
        }

        let wake_ctx = ctx.clone();
        let (midi, midi_rx) = MidiController::open(move || wake_ctx.request_repaint());

        let main_hwnd = {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            cc.window_handle().ok().and_then(|h| match h.as_raw() {
                RawWindowHandle::Win32(w) => Some(windows::Win32::Foundation::HWND(w.hwnd.get() as *mut _)),
                _ => None,
            })
        };

        let mut app = Self {
            device_dialog: DeviceDialog { open: false, hosts: Vec::new(), devices: Vec::new(), choice: engine.current_device() },
            engine,
            history: History::default(),
            snapshots: Snapshots::default(),
            view: View::Suite,
            selected_module: ModuleKind::Equalizer,
            selected_band: None,
            export_format: 0,
            target: 0,
            status: Status::default(),
            busy: Arc::new(AtomicBool::new(false)),
            ctx,
            track_path: None,
            reference_path: None,
            processors,
            processor: 0,
            pre_bypass_mode: ProcessingMode::DspSuite,
            plugin_window: None,
            main_hwnd,
            _midi: Some(midi),
            midi_rx: Some(midi_rx),
            midi_map: HashMap::new(),
            midi_learn: None,
            assistant_report: None,
            screenshot: options.screenshot.map(|p| (p, 0)),
            started: Instant::now(),
        };
        app.status.set("Ready. Open or drop an audio file to start (Space plays, Ctrl+Z undoes).");
        if let Some(kind) = options.module.as_deref().and_then(ModuleKind::from_key) {
            app.selected_module = kind;
        }
        if let Some(file) = options.file {
            // Screenshot runs never start playback
            let play = app.screenshot.is_none();
            app.open_track(&file, play);
        }
        match options.view.as_deref() {
            Some("album") => {
                app.engine.set_montage_mode(true);
                app.refresh_album_playback();
                app.view = View::Album;
            }
            Some("stems") => app.view = View::Stems,
            _ => {}
        }
        Ok(app)
    }

    // -----------------------------------------------------------------------------------------
    // Suite edits (all user changes go through here so they can be undone)
    // -----------------------------------------------------------------------------------------

    /// Apply an edit to the suite, recording an undo step. `control` groups bursts of edits.
    pub(super) fn edit_suite(&mut self, control: &str, f: impl Fn(&mut MasteringSuite) + Send + Sync + 'static) {
        let engine = Arc::clone(&self.engine);
        self.history.record_with(control, || engine.suite_preset());
        self.engine.edit_dsp_suite(f);
    }

    pub(super) fn set_param(&mut self, kind: ModuleKind, index: usize, value: f32) {
        self.edit_suite(&format!("{}/{}", kind.key(), index), move |s| s.module_mut(kind).set_param(index, value));
    }

    fn apply_state(&mut self, preset: &XmlPreset) {
        self.engine.apply_preset(preset);
    }

    pub(super) fn undo(&mut self) {
        if let Some(prev) = self.history.undo(self.engine.suite_preset()) {
            self.apply_state(&prev);
            self.status.set("Undo");
        }
    }

    pub(super) fn redo(&mut self) {
        if let Some(next) = self.history.redo(self.engine.suite_preset()) {
            self.apply_state(&next);
            self.status.set("Redo");
        }
    }

    fn switch_snapshot(&mut self, slot: usize) {
        if slot == self.snapshots.active() {
            return;
        }
        let current = self.engine.suite_preset();
        self.history.record(current.clone(), "snapshot");
        self.history.break_coalescing();
        if let Some(target) = self.snapshots.switch(slot, current) {
            self.apply_state(&target);
        }
        self.status.set(format!("Snapshot {}", SLOT_NAMES[slot]));
    }

    // -----------------------------------------------------------------------------------------
    // Files
    // -----------------------------------------------------------------------------------------

    pub(super) fn open_track(&mut self, path: &Path, play: bool) {
        match load_audio_file(path) {
            Ok(buf) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                self.status.set(format!("{}: {:.1} s, {} Hz", name, buf.duration_seconds(), buf.sample_rate));
                if self.view == View::Album {
                    self.view = View::Suite;
                }
                self.engine.set_audio_buffer(buf);
                self.track_path = Some(path.to_path_buf());
                if play {
                    self.engine.play();
                }
            }
            Err(e) => self.status.set(format!("Could not open {}: {}", path.display(), e)),
        }
    }

    fn pick_audio(title: &str) -> Option<PathBuf> {
        rfd::FileDialog::new().set_title(title).add_filter("Audio", &AUDIO_EXTENSIONS).pick_file()
    }

    fn load_reference(&mut self) {
        let Some(path) = Self::pick_audio("Open reference track") else { return };
        match load_audio_file(&path) {
            Ok(buf) => {
                self.engine.set_reference_buffer(buf);
                self.reference_path = Some(path.clone());
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                match self.engine.auto_match_reference_gain() {
                    Ok((m, r, d)) => self.status.set(format!("Reference {}: track {:.1} / ref {:.1} LUFS (matched {:+.1} dB)", name, m, r, d)),
                    Err(_) => self.status.set(format!("Reference {} loaded", name)),
                }
            }
            Err(e) => self.status.set(format!("Could not open reference: {}", e)),
        }
    }

    fn save_session(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Save session")
            .set_file_name(format!("session.{}", SESSION_EXTENSION))
            .add_filter("Mastering Studio session", &[SESSION_EXTENSION])
            .save_file()
        else {
            return;
        };
        let ui = SessionUiState { export_format: self.export_format, loudness_target: self.target };
        let session = Session::capture(&self.engine, &self.snapshots, self.track_path.as_deref(), self.reference_path.as_deref(), ui);
        match session.save(&path) {
            Ok(()) => self.status.set(format!("Session saved: {}", path.display())),
            Err(e) => self.status.set(e),
        }
    }

    fn open_session(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Open session")
            .add_filter("Mastering Studio session", &[SESSION_EXTENSION])
            .pick_file()
        else {
            return;
        };
        match Session::load(&path) {
            Ok(session) => {
                self.select_processor(0);
                let (warnings, ui) = session.restore(&self.engine, &mut self.snapshots);
                self.track_path = session.track.clone().filter(|p| p.exists());
                self.reference_path = session.reference.clone().filter(|p| p.exists());
                self.export_format = ui.export_format.min(EXPORT_FORMATS.len() - 1);
                self.target = ui.loudness_target.min(TARGETS.len() - 1);
                self.history.clear();
                self.view = View::Suite;
                if warnings.is_empty() {
                    self.status.set(format!("Session opened: {}", path.display()));
                } else {
                    self.status.set(format!("Session opened with problems: {}", warnings.join("; ")));
                }
            }
            Err(e) => self.status.set(e),
        }
    }

    fn save_preset(&mut self) {
        let plugin_mode = self.engine.processing_mode() == ProcessingMode::Vst3 && self.engine.with_vst3_plugin(|p| p.is_some());
        if plugin_mode {
            let Some(path) = rfd::FileDialog::new().set_title("Save plugin state").set_file_name("plugin_state.vststate").add_filter("Plugin state", &["vststate"]).save_file() else { return };
            let res = self.engine.with_vst3_plugin(|p| p.ok_or("No plugin".to_string()).and_then(|p| p.save_state()));
            match res.and_then(|bytes| std::fs::write(&path, bytes).map_err(|e| e.to_string())) {
                Ok(()) => self.status.set("Plugin state saved"),
                Err(e) => self.status.set(format!("Save failed: {}", e)),
            }
            return;
        }
        let Some(path) = rfd::FileDialog::new().set_title("Save suite preset").set_file_name("master_preset.xml").add_filter("Suite preset", &["xml"]).save_file() else { return };
        let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let mut preset = self.engine.suite_preset();
        preset.name = name.clone();
        match preset.save_to_file(&path) {
            Ok(()) => self.status.set(format!("Preset saved: {}", name)),
            Err(e) => self.status.set(e),
        }
    }

    fn load_preset(&mut self) {
        let Some(path) = rfd::FileDialog::new().set_title("Load preset").add_filter("Presets", &["xml", "vststate"]).pick_file() else { return };
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        if ext == "vststate" {
            let res = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|bytes| {
                self.engine.with_vst3_plugin(|p| p.ok_or("Load a VST3 plugin first".to_string()).and_then(|p| p.load_state(&bytes)))
            });
            match res {
                Ok(()) => self.status.set("Plugin state loaded"),
                Err(e) => self.status.set(format!("Load failed: {}", e)),
            }
            return;
        }
        let before = self.engine.suite_preset();
        match self.engine.load_xml_preset(&path) {
            Ok(p) => {
                self.history.record(before, "preset");
                self.history.break_coalescing();
                self.status.set(format!("Preset loaded: {}", p.name));
            }
            Err(e) => self.status.set(e),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Processing modes and fallback plugins
    // -----------------------------------------------------------------------------------------

    fn close_plugin_window(&mut self) {
        if let Some(w) = self.plugin_window.take() {
            self.engine.with_vst3_plugin(|p| w.close(p));
        }
    }

    pub(super) fn select_processor(&mut self, index: usize) {
        if self.busy.load(Ordering::Relaxed) {
            self.status.set("Wait for the running job to finish before switching processors");
            return;
        }
        self.close_plugin_window();
        let _ = self.engine.set_vst3_plugin(None);
        match self.processors.get(index).cloned() {
            Some(Processor::Vst3(name, path)) => match Vst3Plugin::load(&path) {
                Ok(plugin) => {
                    if let Err(e) = self.engine.set_vst3_plugin(Some(plugin)) {
                        self.status.set(format!("Plugin setup failed: {}", e));
                        return;
                    }
                    self.engine.set_processing_mode(ProcessingMode::Vst3);
                    self.pre_bypass_mode = ProcessingMode::Vst3;
                    self.processor = index;
                    self.open_plugin_window();
                    self.status.set(format!("Fallback plugin: {}", name));
                }
                Err(e) => self.status.set(format!("Could not load {}: {}", path.display(), e)),
            },
            _ => {
                self.engine.set_processing_mode(ProcessingMode::DspSuite);
                self.pre_bypass_mode = ProcessingMode::DspSuite;
                self.processor = 0;
            }
        }
    }

    fn open_plugin_window(&mut self) {
        if let Some(ref w) = self.plugin_window {
            w.show();
            return;
        }
        let owner = self.main_hwnd;
        let res = self.engine.with_vst3_plugin(|p| p.map(|p| PluginWindow::open(owner, p)));
        match res {
            Some(Ok(w)) => self.plugin_window = Some(w),
            Some(Err(e)) => self.status.set(format!("The plugin has no editor window: {}", e)),
            None => {}
        }
    }

    fn browse_vst3(&mut self) {
        let Some(path) = rfd::FileDialog::new().set_title("Load a VST3 plugin (fallback)").add_filter("VST3", &["vst3"]).pick_file() else { return };
        let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        self.processors.push(Processor::Vst3(name, path));
        self.select_processor(self.processors.len() - 1);
    }

    fn toggle_bypass(&mut self) {
        let mode = self.engine.processing_mode();
        if mode == ProcessingMode::Bypass {
            self.engine.set_processing_mode(self.pre_bypass_mode);
            self.status.set("A/B: processed");
        } else {
            self.pre_bypass_mode = mode;
            self.engine.set_processing_mode(ProcessingMode::Bypass);
            self.status.set("A/B: bypass (dry)");
        }
    }

    pub(super) fn apply_target(&mut self) {
        let t = TARGETS[self.target];
        let before = self.engine.suite_preset();
        match self.engine.apply_loudness_target(t.target_lufs, t.ceiling_dbtp) {
            Ok((lufs, boost)) => {
                self.history.record(before, "target");
                self.history.break_coalescing();
                self.status.set(format!("{}: track {:.1} LUFS, maximizer drive {:+.1} dB, ceiling {:.1} dBTP", t.name, lufs, boost, t.ceiling_dbtp));
            }
            Err(e) => self.status.set(format!("{}: {}", t.name, e)),
        }
    }

    fn run_assistant(&mut self) {
        let before = self.engine.suite_preset();
        match self.engine.run_master_assistant(TARGETS[self.target].target_lufs) {
            Ok(r) => {
                self.history.record(before, "assistant");
                self.history.break_coalescing();
                let eq: String = r.recommended_eq_adjustments.iter().map(|l| format!("  • {}\n", l)).collect();
                self.assistant_report = Some(format!(
                    "Track profile\n  • Integrated {:.1} LUFS (target {:.1})\n  • Peak {:.1} dBTP, RMS {:.1} dB, crest {:.1} dB\n  • Sub-bass phase correlation {:+.2}\n\nApplied\n  • Maximizer threshold {:.1} dB, ceiling {:.1} dBTP\n  • Stereo width {:.0}%, mono below {:.0} Hz\n\nEQ moves\n{}",
                    r.current_integrated_lufs, r.target_integrated_lufs, r.peak_level_dbtp, r.rms_level_db, r.crest_factor_db,
                    r.sub_bass_phase_corr, r.recommended_threshold_db, r.recommended_ceiling_db, r.recommended_stereo_width * 100.0,
                    r.recommended_mono_maker_hz, eq
                ));
            }
            Err(e) => self.status.set(format!("Assistant: {}", e)),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Background jobs
    // -----------------------------------------------------------------------------------------

    /// Run `job` on a worker thread unless another job is running
    pub(super) fn spawn_job(&self, job: impl FnOnce(&AudioEngine, &Status) + Send + 'static) {
        if self.busy.swap(true, Ordering::AcqRel) {
            self.status.set("Another job is still running");
            return;
        }
        let engine = Arc::clone(&self.engine);
        let status = self.status.clone();
        let busy = Arc::clone(&self.busy);
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            job(&engine, &status);
            busy.store(false, Ordering::Release);
            ctx.request_repaint();
        });
    }

    fn export(&mut self) {
        let Some(buf) = self.engine.audio_buffer() else {
            self.status.set("Load a track to export");
            return;
        };
        let format = EXPORT_FORMATS[self.export_format];
        let Some(path) = rfd::FileDialog::new()
            .set_title(format!("Export master ({})", format.name))
            .set_file_name("master.wav")
            .add_filter("WAV", &["wav"])
            .save_file()
        else {
            return;
        };
        self.engine.pause();
        let mode = self.engine.processing_mode();
        let ctx = self.ctx.clone();
        self.spawn_job(move |engine, status| {
            let trim = engine.trim_fade();
            let stems = engine.ml_stems();
            let res = with_render_source(engine, mode, |source| {
                let opts = RenderOptions {
                    trim_fade: Some(trim),
                    bwf_metadata: Some(BwfMetadata::default()),
                    target_sample_rate: format.sample_rate,
                    stems: stems.clone(),
                    ..Default::default()
                };
                render_to_wav(&buf, &path, format.bit_depth, source, Some(opts), None, |p| {
                    status.set(format!("Rendering master: {:.0}%", p * 100.0));
                    ctx.request_repaint();
                })
            });
            match res {
                Ok(()) => status.set(format!("Exported {}", path.display())),
                Err(e) => status.set(format!("Export failed: {}", e)),
            }
        });
    }

    fn batch(&mut self) {
        let Some(folder) = rfd::FileDialog::new().set_title("Master every audio file in a folder").pick_folder() else { return };
        let files: Vec<PathBuf> = std::fs::read_dir(&folder)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().and_then(|e| e.to_str()).map(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str())).unwrap_or(false))
                    .collect()
            })
            .unwrap_or_default();
        if files.is_empty() {
            self.status.set("No audio files in that folder");
            return;
        }
        self.engine.pause();
        let mode = self.engine.processing_mode();
        let format = EXPORT_FORMATS[self.export_format];
        let out_dir = folder.join("Mastered");
        self.spawn_job(move |engine, status| {
            let _ = std::fs::create_dir_all(&out_dir);
            let total = files.len();
            let mut done = 0;
            for (i, file) in files.iter().enumerate() {
                let stem = file.file_stem().unwrap_or_default().to_string_lossy().to_string();
                status.set(format!("Batch {}/{}: {}", i + 1, total, stem));
                if let Ok(buf) = load_audio_file(file) {
                    let out = out_dir.join(format!("{}_master.wav", stem));
                    let res = with_render_source(engine, mode, |source| {
                        let opts = RenderOptions { bwf_metadata: Some(BwfMetadata::default()), target_sample_rate: format.sample_rate, ..Default::default() };
                        render_to_wav(&buf, &out, format.bit_depth, source, Some(opts), None, |_| {})
                    });
                    if res.is_ok() {
                        done += 1;
                    }
                }
            }
            status.set(format!("Batch done: {}/{} files in {}", done, total, out_dir.display()));
        });
    }

    pub(super) fn separate_stems_ai(&self) {
        let ctx = self.ctx.clone();
        self.spawn_job(move |engine, status| {
            let res = engine.separate_with_model(|p| {
                status.set(format!("Separating stems with the trained model: {:.0}%", p * 100.0));
                ctx.request_repaint();
            });
            match res {
                Ok(_) => status.set("Stems ready: Stem Rebalance and Stem EQ now use the separated stems"),
                Err(e) => status.set(format!("Stem separation: {}", e)),
            }
        });
    }

    pub(super) fn learn_match_eq(&mut self) {
        let before = self.engine.suite_preset();
        match self.engine.learn_match_eq() {
            Ok(()) => {
                self.history.record(before, "match-learn");
                self.history.break_coalescing();
                self.status.set("Match EQ learned the reference's tonal balance and is now on");
            }
            Err(e) => self.status.set(format!("Match EQ: {}", e)),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Input
    // -----------------------------------------------------------------------------------------

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        use egui::Key;
        let (pressed, ctrl, shift): (Vec<Key>, bool, bool) = ctx.input(|i| {
            let keys = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                    _ => None,
                })
                .collect();
            (keys, i.modifiers.command, i.modifiers.shift)
        });
        for key in pressed {
            match (key, ctrl) {
                (Key::Z, true) if shift => self.redo(),
                (Key::Z, true) => self.undo(),
                (Key::Y, true) => self.redo(),
                (Key::O, true) => {
                    if let Some(p) = Self::pick_audio("Open audio file") {
                        self.open_track(&p, false);
                    }
                }
                (Key::E, true) => self.export(),
                (Key::S, true) => self.save_session(),
                (Key::Num1, true) => self.switch_snapshot(0),
                (Key::Num2, true) => self.switch_snapshot(1),
                (Key::Num3, true) => self.switch_snapshot(2),
                (Key::Num4, true) => self.switch_snapshot(3),
                (Key::Space, false) => self.toggle_play(),
                (Key::Escape, false) => self.engine.stop(),
                (Key::ArrowLeft, false) => self.nudge(-5.0),
                (Key::ArrowRight, false) => self.nudge(5.0),
                (Key::B, false) => self.toggle_bypass(),
                (Key::G, false) => self.engine.set_ab_gain_match(!self.engine.ab_gain_match()),
                (Key::L, false) => self.engine.set_loop(!self.engine.is_looping()),
                (Key::R, false) => self.engine.set_referencing(!self.engine.is_referencing()),
                (Key::A, false) => self.run_assistant(),
                (Key::T, false) => {
                    self.target = (self.target + 1) % TARGETS.len();
                    self.apply_target();
                }
                (Key::D, false) => self.export_format = (self.export_format + 1) % EXPORT_FORMATS.len(),
                (Key::H, false) => self.open_device_dialog(),
                (Key::OpenBracket, false) => self.set_trim_here(true),
                (Key::CloseBracket, false) => self.set_trim_here(false),
                (Key::F, false) => self.cycle_fades(),
                (Key::C, false) => {
                    self.engine.set_trim_start_fraction(0.0);
                    self.engine.set_trim_end_fraction(1.0);
                    self.engine.set_fade_in_seconds(0.0);
                    self.engine.set_fade_out_seconds(0.0);
                    self.status.set("Trim and fades cleared");
                }
                _ => {}
            }
        }
    }

    fn toggle_play(&self) {
        if self.engine.is_playing() {
            self.engine.pause();
        } else {
            self.engine.play();
        }
    }

    fn nudge(&self, seconds: f64) {
        let dur = self.engine.duration_seconds();
        if dur > 0.0 {
            let t = (self.engine.current_position_seconds() + seconds).clamp(0.0, dur);
            self.engine.seek_fraction(t / dur);
        }
    }

    fn set_trim_here(&self, start: bool) {
        let dur = self.engine.duration_seconds();
        if dur <= 0.0 {
            return;
        }
        let frac = (self.engine.current_position_seconds() / dur).clamp(0.0, 1.0);
        if start {
            self.engine.set_trim_start_fraction(frac);
        } else {
            self.engine.set_trim_end_fraction(frac);
        }
        self.status.set(format!("Trim {} at {:.1}%", if start { "in" } else { "out" }, frac * 100.0));
    }

    fn cycle_fades(&self) {
        let tf = self.engine.trim_fade();
        let (secs, shape, desc) = if tf.fade_in_seconds < 0.01 {
            (1.5, FadeShape::SCurve, "1.5 s S-curve")
        } else if tf.fade_in_seconds < 2.0 {
            (3.0, FadeShape::Exponential, "3 s exponential")
        } else if tf.fade_in_seconds < 4.0 {
            (5.0, FadeShape::Linear, "5 s linear")
        } else {
            (0.0, FadeShape::SCurve, "off")
        };
        let mut next = tf;
        next.fade_in_seconds = secs;
        next.fade_out_seconds = secs;
        next.fade_shape = shape;
        self.engine.set_trim_fade(next);
        self.status.set(format!("Fades: {}", desc));
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let files: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if files.is_empty() {
            return;
        }
        if self.view == View::Album {
            self.add_album_tracks(files);
        } else if let Some(first) = files.first() {
            let ext = first.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
            if ext == SESSION_EXTENSION {
                self.status.set("Use Session ▸ Open to open session files");
            } else {
                let first = first.clone();
                self.open_track(&first, true);
            }
        }
    }

    pub(super) fn add_album_tracks(&mut self, files: Vec<PathBuf>) {
        let mut added = 0;
        for p in files {
            if let Ok(buf) = load_audio_file(&p) {
                let lufs = calculate_buffer_integrated_lufs(&buf);
                let tp = buf.peaks().iter().fold(0.0001f32, |a, pk| a.max(pk.max_l).max(pk.max_r));
                let (dur, sr) = (buf.duration_seconds(), buf.sample_rate);
                self.engine.with_montage(|m| {
                    m.add_track(p.clone(), dur, sr);
                    if let Some(last) = m.tracks.last_mut() {
                        last.integrated_lufs = lufs;
                        last.true_peak_dbtp = (20.0 * tp.log10()).clamp(-60.0, 6.0);
                    }
                });
                added += 1;
            }
        }
        if added > 0 {
            self.refresh_album_playback();
        }
    }

    /// Re-sequence the album for playback (decodes every track, so it runs on a worker)
    pub(super) fn refresh_album_playback(&self) {
        let engine = Arc::clone(&self.engine);
        let status = self.status.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            status.set("Album: sequencing for playback...");
            match engine.refresh_montage_playback() {
                Ok(()) if engine.duration_seconds() <= 0.0 => status.set("Album is empty: add tracks to play it"),
                Ok(()) => status.set(format!("Album ready for playback: {:.1} s", engine.duration_seconds())),
                Err(e) => status.set(format!("Album playback: {}", e)),
            }
            ctx.request_repaint();
        });
    }

    fn handle_midi(&mut self) {
        let Some(rx) = self.midi_rx.clone() else { return };
        while let Ok(event) = rx.try_recv() {
            match event {
                MidiEvent::ControlChange { channel, controller, value } => {
                    if let Some(target) = self.midi_learn.take() {
                        self.midi_map.insert((channel, controller), target);
                        self.status.set(format!("MIDI learn: CC {} (channel {}) mapped", controller, channel + 1));
                        continue;
                    }
                    let norm = value as f32 / 127.0;
                    match self.midi_map.get(&(channel, controller)).copied() {
                        Some(MidiTarget::Param(kind, index)) => {
                            let info = self.engine.with_dsp_suite(|s| s.module(kind).params().get(index).cloned());
                            if let Some(info) = info {
                                let v = if info.log {
                                    info.min * (info.max / info.min.max(1e-6)).powf(norm)
                                } else {
                                    info.min + (info.max - info.min) * norm
                                };
                                self.set_param(kind, index, v);
                            }
                        }
                        None => match controller {
                            // Defaults when nothing is learned on this CC
                            1 => self.set_param(ModuleKind::Exciter, 1, norm),
                            7 => self.set_param(ModuleKind::Maximizer, 0, norm * 24.0),
                            10 => self.set_param(ModuleKind::Imager, 0, norm * 2.0),
                            64 if value >= 64 => self.toggle_play(),
                            80 if value >= 64 => self.engine.set_referencing(!self.engine.is_referencing()),
                            _ => {}
                        },
                    }
                }
                MidiEvent::NoteOn { note, .. } => match note {
                    36 => self.toggle_play(),
                    38 => self.engine.stop(),
                    40 => self.engine.set_referencing(!self.engine.is_referencing()),
                    41 => self.toggle_bypass(),
                    _ => {}
                },
            }
        }
    }

    fn open_device_dialog(&mut self) {
        self.device_dialog.hosts = list_audio_hosts();
        self.device_dialog.choice = self.engine.current_device();
        self.device_dialog.devices = list_output_devices(self.device_dialog.choice.host.as_deref());
        self.device_dialog.open = true;
    }

    // -----------------------------------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------------------------------

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy.load(Ordering::Relaxed);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Open…").on_hover_text("Open an audio file (Ctrl+O)").clicked() {
                if let Some(p) = Self::pick_audio("Open audio file") {
                    self.open_track(&p, false);
                }
            }
            if ui.add_enabled(!busy, egui::Button::new("Export…")).on_hover_text("Render the master (Ctrl+E)").clicked() {
                self.export();
            }
            if ui.add_enabled(!busy, egui::Button::new("Batch…")).on_hover_text("Master every file in a folder").clicked() {
                self.batch();
            }
            ui.menu_button("Session", |ui| {
                if ui.button("Save session…  (Ctrl+S)").clicked() {
                    ui.close_menu();
                    self.save_session();
                }
                if ui.button("Open session…").clicked() {
                    ui.close_menu();
                    self.open_session();
                }
            });
            ui.menu_button("Preset", |ui| {
                if ui.button("Save preset…").clicked() {
                    ui.close_menu();
                    self.save_preset();
                }
                if ui.button("Load preset…").clicked() {
                    ui.close_menu();
                    self.load_preset();
                }
            });
            ui.separator();
            if ui.add_enabled(self.history.can_undo(), egui::Button::new("⟲ Undo")).on_hover_text("Ctrl+Z").clicked() {
                self.undo();
            }
            if ui.add_enabled(self.history.can_redo(), egui::Button::new("⟳ Redo")).on_hover_text("Ctrl+Y").clicked() {
                self.redo();
            }
            ui.separator();
            ui.label(RichText::new("Snapshot").color(DIM));
            for (i, name) in SLOT_NAMES.iter().enumerate() {
                let active = self.snapshots.active() == i;
                let mut text = RichText::new(*name).strong();
                if !active && !self.snapshots.is_filled(i) {
                    text = text.color(DIM);
                }
                let resp = ui.selectable_label(active, text).on_hover_text(format!("Ctrl+{}. Right-click to copy the current settings here", i + 1));
                if resp.clicked() {
                    self.switch_snapshot(i);
                }
                resp.context_menu(|ui| {
                    if ui.button(format!("Copy current settings to {}", name)).clicked() {
                        self.snapshots.copy_to(i, self.engine.suite_preset());
                        ui.close_menu();
                    }
                });
            }
            ui.separator();
            egui::ComboBox::from_id_salt("target")
                .selected_text(TARGETS[self.target].name)
                .width(170.0)
                .show_ui(ui, |ui| {
                    for (i, t) in TARGETS.iter().enumerate() {
                        if ui.selectable_label(self.target == i, t.name).clicked() {
                            self.target = i;
                            self.apply_target();
                        }
                    }
                })
                .response
                .on_hover_text("Loudness target: sets maximizer drive and ceiling (T)");
            egui::ComboBox::from_id_salt("format")
                .selected_text(EXPORT_FORMATS[self.export_format].name)
                .width(190.0)
                .show_ui(ui, |ui| {
                    for (i, f) in EXPORT_FORMATS.iter().enumerate() {
                        ui.selectable_value(&mut self.export_format, i, f.name);
                    }
                })
                .response
                .on_hover_text("Export format (D)");
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Processor").color(DIM));
            let current = match &self.processors[self.processor] {
                Processor::BuiltIn => "Built-in suite".to_string(),
                Processor::Vst3(n, _) => format!("VST3: {}", n),
            };
            let mut choose = None;
            let mut browse = false;
            egui::ComboBox::from_id_salt("processor").selected_text(current).width(220.0).show_ui(ui, |ui| {
                for (i, p) in self.processors.iter().enumerate() {
                    let label = match p {
                        Processor::BuiltIn => "Built-in suite".to_string(),
                        Processor::Vst3(n, _) => format!("VST3 (fallback): {}", n),
                    };
                    if ui.selectable_label(self.processor == i, label).clicked() {
                        choose = Some(i);
                    }
                }
                if ui.selectable_label(false, "Load VST3 plugin…").clicked() {
                    browse = true;
                }
            });
            if let Some(i) = choose {
                if i != self.processor {
                    self.select_processor(i);
                }
            }
            if browse {
                self.browse_vst3();
            }
            if matches!(self.processors[self.processor], Processor::Vst3(..)) && ui.button("Plugin editor").clicked() {
                self.open_plugin_window();
            }
            ui.separator();
            let bypassed = self.engine.processing_mode() == ProcessingMode::Bypass;
            if ui.selectable_label(bypassed, if bypassed { "A/B: DRY" } else { "A/B: processed" }).on_hover_text("Bypass everything (B)").clicked() {
                self.toggle_bypass();
            }
            let mut matched = self.engine.ab_gain_match();
            if ui.toggle_value(&mut matched, "Loudness-matched A/B").on_hover_text("Play the processed signal at the input's loudness (G)").changed() {
                self.engine.set_ab_gain_match(matched);
            }
            ui.separator();
            if ui.button("+ Reference…").clicked() {
                self.load_reference();
            }
            let has_ref = self.engine.reference_buffer().is_some();
            let mut referencing = self.engine.is_referencing();
            if ui.add_enabled(has_ref, egui::SelectableLabel::new(referencing, "Hear reference")).on_hover_text("R").clicked() {
                referencing = !referencing;
                self.engine.set_referencing(referencing);
            }
            if ui.add_enabled(has_ref, egui::Button::new("Match ref loudness")).clicked() {
                match self.engine.auto_match_reference_gain() {
                    Ok((m, r, d)) => self.status.set(format!("Reference matched: track {:.1} / ref {:.1} LUFS ({:+.1} dB)", m, r, d)),
                    Err(e) => self.status.set(e),
                }
            }
            ui.separator();
            if ui.button("Assistant").on_hover_text("Analyse the track and set up the suite (A)").clicked() {
                self.run_assistant();
            }
            if ui.button("Audio device…").on_hover_text("H").clicked() {
                self.open_device_dialog();
            }
        });
    }

    fn transport(&mut self, ui: &mut egui::Ui, meters: &crate::audio::MeterLevels) {
        ui.horizontal(|ui| {
            let playing = self.engine.is_playing();
            if ui.add_sized([40.0, 30.0], egui::Button::new(if playing { "⏸" } else { "▶" })).on_hover_text("Space").clicked() {
                self.toggle_play();
            }
            if ui.add_sized([40.0, 30.0], egui::Button::new("⏹")).on_hover_text("Esc").clicked() {
                self.engine.stop();
            }
            let mut looping = self.engine.is_looping();
            if ui.add_sized([46.0, 30.0], egui::SelectableLabel::new(looping, "Loop")).clicked() {
                looping = !looping;
                self.engine.set_loop(looping);
            }

            let dur = self.engine.duration_seconds();
            let pos = self.engine.current_position_seconds();
            let buf = if self.engine.is_referencing() { self.engine.reference_buffer() } else { self.engine.audio_buffer() };
            let trim = self.engine.trim_fade();
            let width = (ui.available_width() - 470.0).max(200.0);
            let frac = if dur > 0.0 { pos / dur } else { 0.0 };
            match widgets::waveform(ui, buf.as_deref(), frac, &trim, self.engine.is_referencing(), Vec2::new(width, 46.0)) {
                WaveformAction::Seek(f) => self.engine.seek_fraction(f),
                WaveformAction::TrimIn(f) => self.engine.set_trim_start_fraction(f),
                WaveformAction::TrimOut(f) => self.engine.set_trim_end_fraction(f),
                WaveformAction::None => {}
            }
            let t = |s: f64| format!("{:02}:{:05.2}", (s / 60.0) as u32, s % 60.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(format!("{} / {}", t(pos), t(dur))).monospace().size(14.0));
                let mut loud = format!("I {:5.1}  S {:5.1} LUFS  TP {:5.1}", meters.integrated_lufs, meters.short_term_lufs, meters.true_peak_l_dbtp.max(meters.true_peak_r_dbtp));
                if self.engine.ab_gain_match() {
                    loud.push_str(&format!("  match {:+.1} dB", meters.ab_match_db));
                }
                ui.label(RichText::new(loud).monospace().size(12.0).color(ACCENT));
            });
            widgets::level_meters(ui, meters, Vec2::new(170.0, 44.0));
        });
    }

    fn device_window(&mut self, ctx: &egui::Context) {
        if !self.device_dialog.open {
            return;
        }
        let mut open = true;
        let mut apply = false;
        egui::Window::new("Audio device").open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
            let info = self.engine.get_device_info();
            ui.label(format!("Now: {} via {} at {} Hz, {} ch, {}", info.name, info.host, info.sample_rate, info.channels, info.sample_format));
            ui.separator();
            let d = &mut self.device_dialog;
            let mut host_changed = false;
            egui::Grid::new("device_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Driver");
                egui::ComboBox::from_id_salt("host").selected_text(d.choice.host.clone().unwrap_or_else(|| "Default".into())).show_ui(ui, |ui| {
                    for h in &d.hosts {
                        if ui.selectable_label(d.choice.host.as_ref() == Some(h), h).clicked() {
                            d.choice.host = Some(h.clone());
                            d.choice.device = None;
                            host_changed = true;
                        }
                    }
                });
                ui.end_row();
                ui.label("Output");
                egui::ComboBox::from_id_salt("dev").width(320.0).selected_text(d.choice.device.clone().unwrap_or_else(|| "Default".into())).show_ui(ui, |ui| {
                    if ui.selectable_label(d.choice.device.is_none(), "Default").clicked() {
                        d.choice.device = None;
                    }
                    for dev in &d.devices {
                        if ui.selectable_label(d.choice.device.as_ref() == Some(&dev.name), &dev.name).clicked() {
                            d.choice.device = Some(dev.name.clone());
                            d.choice.sample_rate = None;
                        }
                    }
                });
                ui.end_row();
                let selected = d.devices.iter().find(|x| Some(&x.name) == d.choice.device.as_ref()).or_else(|| d.devices.first());
                ui.label("Sample rate");
                let rates: Vec<u32> = selected.map(|s| s.sample_rates.clone()).unwrap_or_default();
                egui::ComboBox::from_id_salt("rate")
                    .selected_text(d.choice.sample_rate.map(|r| format!("{} Hz", r)).unwrap_or_else(|| "Device default".into()))
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(d.choice.sample_rate.is_none(), "Device default").clicked() {
                            d.choice.sample_rate = None;
                        }
                        for r in rates {
                            if ui.selectable_label(d.choice.sample_rate == Some(r), format!("{} Hz", r)).clicked() {
                                d.choice.sample_rate = Some(r);
                            }
                        }
                    });
                ui.end_row();
                ui.label("Buffer size");
                let range = selected.and_then(|s| s.buffer_range);
                egui::ComboBox::from_id_salt("buf")
                    .selected_text(d.choice.buffer_frames.map(|b| format!("{} frames", b)).unwrap_or_else(|| "Driver default".into()))
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(d.choice.buffer_frames.is_none(), "Driver default").clicked() {
                            d.choice.buffer_frames = None;
                        }
                        for b in [64u32, 128, 256, 512, 1024, 2048, 4096] {
                            let ok = range.map(|(lo, hi)| b >= lo && b <= hi).unwrap_or(true);
                            if ok && ui.selectable_label(d.choice.buffer_frames == Some(b), format!("{} frames", b)).clicked() {
                                d.choice.buffer_frames = Some(b);
                            }
                        }
                    });
                ui.end_row();
            });
            if host_changed {
                d.devices = list_output_devices(d.choice.host.as_deref());
            }
            if !d.hosts.iter().any(|h| h.eq_ignore_ascii_case("asio")) {
                ui.label(RichText::new("ASIO: build with `--features asio` (needs the Steinberg ASIO SDK)").color(DIM).size(11.0));
            }
            ui.separator();
            if ui.button("Apply").clicked() {
                apply = true;
            }
        });
        if apply {
            let choice = self.device_dialog.choice.clone();
            match self.engine.reopen(&choice) {
                Ok(()) => self.status.set(format!("Audio output: {} Hz", self.engine.device_sample_rate())),
                Err(e) => self.status.set(format!("Could not open that output ({}); kept the previous one", e)),
            }
        }
        self.device_dialog.open = open;
    }

    fn assistant_window(&mut self, ctx: &egui::Context) {
        let Some(text) = self.assistant_report.clone() else { return };
        let mut open = true;
        egui::Window::new("Master Assistant").open(&mut open).resizable(false).show(ctx, |ui| {
            ui.label(RichText::new(text).monospace());
        });
        if !open {
            self.assistant_report = None;
        }
    }

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, frames)) = self.screenshot.as_mut() else { return };
        *frames += 1;
        // Give the meters and layout a moment to settle
        if *frames == 40 || (*frames > 40 && *frames % 60 == 0) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let path = path.clone();
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(Arc::clone(image)),
                _ => None,
            })
        });
        if let Some(image) = shot {
            let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
            match save_png(&path, image.size[0], image.size[1], &rgba) {
                Ok(()) => println!("Screenshot saved to {}", path.display()),
                Err(e) => eprintln!("Screenshot failed: {}", e),
            }
            self.screenshot = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint();
    }
}

impl eframe::App for MasteringApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.engine.collect_garbage();
        self.handle_midi();
        self.handle_drops(ctx);
        self.handle_keys(ctx);
        let meters = self.engine.get_meter_levels();

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(2.0);
            ui.separator();
            self.transport(ui, &meters);
            ui.add_space(4.0);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let busy = self.busy.load(Ordering::Relaxed);
                if busy {
                    ui.spinner();
                }
                ui.label(RichText::new(self.status.get()).color(if busy { WARN } else { DIM }));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.midi_learn.is_some() {
                        ui.label(RichText::new("MIDI learn: move a controller…").color(WARN));
                    }
                    let info = self.engine.get_device_info();
                    ui.label(RichText::new(format!("{} • {} Hz", info.name, info.sample_rate)).color(DIM).size(11.0));
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (view, label) in [(View::Suite, "Mastering suite"), (View::Album, "Album"), (View::Stems, "Stems")] {
                    if ui.selectable_label(self.view == view, RichText::new(label).size(15.0)).clicked() && self.view != view {
                        // The album tab plays the sequenced album; other tabs play the single track
                        if view == View::Album {
                            self.engine.set_montage_mode(true);
                            self.refresh_album_playback();
                        } else if self.view == View::Album {
                            self.engine.set_montage_mode(false);
                        }
                        self.view = view;
                    }
                }
            });
            ui.separator();
            match self.view {
                View::Suite => super::suite_view::show(self, ui, &meters),
                View::Album => super::album_view::show(self, ui),
                View::Stems => super::stems_view::show(self, ui),
            }
        });

        self.device_window(ctx);
        self.assistant_window(ctx);
        self.handle_screenshot(ctx);

        let busy = self.busy.load(Ordering::Relaxed);
        let fast = self.engine.is_playing() || busy || self.started.elapsed() < Duration::from_secs(2);
        ctx.request_repaint_after(Duration::from_millis(if fast { 16 } else { 120 }));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.close_plugin_window();
        let _ = self.engine.set_vst3_plugin(None);
        self.engine.pause();
        self.engine.collect_garbage();
    }
}

/// Run `f` with the render source for the current processing mode; a fallback plugin is taken
/// off the audio thread for the duration
pub(super) fn with_render_source<R>(engine: &AudioEngine, mode: ProcessingMode, f: impl for<'a> FnOnce(RenderSource<'a>) -> R) -> R {
    let mut suite = engine.dsp_suite_snapshot();
    engine.with_detached_vst3_plugin(|plugin| {
        let source = match mode {
            ProcessingMode::Vst3 => match plugin {
                Some(p) => RenderSource::Vst3(p),
                None => RenderSource::Bypass,
            },
            ProcessingMode::DspSuite => RenderSource::Dsp(&mut suite),
            ProcessingMode::Bypass => RenderSource::Bypass,
        };
        f(source)
    })
}
