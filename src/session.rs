//! Undo/redo, A/B/C/D snapshots and session files.
//!
//! Suite states are captured as [`XmlPreset`]s (every module's on/off state and parameters, the
//! chain order, auto gain and learned Match EQ) and restored with `AudioEngine::apply_preset`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::audio::{load_audio_file, AudioEngine, FadeShape, TrimFadeSettings};
use crate::dsp::XmlPreset;

const HISTORY_LIMIT: usize = 200;
/// Edits to the same control closer together than this become one undo step (wheel spins, drags)
const COALESCE_WINDOW: Duration = Duration::from_millis(700);

/// Undo/redo of suite settings
#[derive(Default)]
pub struct History {
    undo: Vec<XmlPreset>,
    redo: Vec<XmlPreset>,
    last: Option<(String, Instant)>,
}

impl History {
    /// Record the state *before* an edit. `control` identifies what is being edited, so a
    /// burst of edits to the same control collapses into one step.
    pub fn record(&mut self, before: XmlPreset, control: &str) {
        self.record_with(control, || before);
    }

    /// Like `record`, but only builds the before-state when the edit starts a new step
    pub fn record_with(&mut self, control: &str, before: impl FnOnce() -> XmlPreset) {
        let now = Instant::now();
        let coalesce = matches!(&self.last, Some((c, t)) if c == control && now.duration_since(*t) < COALESCE_WINDOW);
        self.last = Some((control.to_string(), now));
        if coalesce {
            return;
        }
        self.undo.push(before());
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// End the current burst so the next edit starts a new step
    pub fn break_coalescing(&mut self) {
        self.last = None;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Step back: returns the state to restore, given the current one
    pub fn undo(&mut self, current: XmlPreset) -> Option<XmlPreset> {
        let prev = self.undo.pop()?;
        self.redo.push(current);
        self.last = None;
        Some(prev)
    }

    pub fn redo(&mut self, current: XmlPreset) -> Option<XmlPreset> {
        let next = self.redo.pop()?;
        self.undo.push(current);
        self.last = None;
        Some(next)
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.last = None;
    }
}

pub const SLOT_NAMES: [&str; 4] = ["A", "B", "C", "D"];

/// Four snapshot slots for comparing whole-chain settings. The active slot follows edits, so
/// switching away and back returns to where you left it.
pub struct Snapshots {
    slots: [Option<XmlPreset>; 4],
    active: usize,
}

impl Default for Snapshots {
    fn default() -> Self {
        Self { slots: [None, None, None, None], active: 0 }
    }
}

impl Snapshots {
    pub fn active(&self) -> usize {
        self.active
    }

    pub fn is_filled(&self, slot: usize) -> bool {
        self.slots.get(slot).map(|s| s.is_some()).unwrap_or(false)
    }

    /// Switch to `slot`: stores `current` in the active slot and returns the target's state
    /// (None when the target is empty: it then starts as a copy of the current settings)
    pub fn switch(&mut self, slot: usize, current: XmlPreset) -> Option<XmlPreset> {
        if slot >= 4 || slot == self.active {
            return None;
        }
        let target = self.slots[slot].take();
        self.slots[self.active] = Some(current.clone());
        self.active = slot;
        // The new active slot holds whatever is live; keep a copy so it isn't empty
        self.slots[slot] = Some(target.clone().unwrap_or(current));
        target
    }

    /// Copy the current settings into another slot without switching
    pub fn copy_to(&mut self, slot: usize, current: XmlPreset) {
        if slot < 4 {
            self.slots[slot] = Some(current);
        }
    }

    pub fn clear(&mut self) {
        self.slots = [None, None, None, None];
        self.active = 0;
    }
}

// ---------------------------------------------------------------------------------------------
// Session files
// ---------------------------------------------------------------------------------------------

pub const SESSION_EXTENSION: &str = "mstsession";
const SESSION_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SessionTrim {
    pub start_fraction: f64,
    pub end_fraction: f64,
    pub fade_in_seconds: f64,
    pub fade_out_seconds: f64,
    /// "linear", "exponential" or "s-curve"
    pub fade_shape: String,
}

impl SessionTrim {
    fn from_settings(t: &TrimFadeSettings) -> Self {
        Self {
            start_fraction: t.trim_start_fraction,
            end_fraction: t.trim_end_fraction,
            fade_in_seconds: t.fade_in_seconds,
            fade_out_seconds: t.fade_out_seconds,
            fade_shape: match t.fade_shape {
                FadeShape::Linear => "linear",
                FadeShape::Exponential => "exponential",
                FadeShape::SCurve => "s-curve",
            }
            .to_string(),
        }
    }

    fn to_settings(&self) -> TrimFadeSettings {
        TrimFadeSettings {
            trim_start_fraction: self.start_fraction.clamp(0.0, 1.0),
            trim_end_fraction: self.end_fraction.clamp(0.0, 1.0),
            fade_in_seconds: self.fade_in_seconds.max(0.0),
            fade_out_seconds: self.fade_out_seconds.max(0.0),
            fade_shape: match self.fade_shape.as_str() {
                "linear" => FadeShape::Linear,
                "exponential" => FadeShape::Exponential,
                _ => FadeShape::SCurve,
            },
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionMontageTrack {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub duration_seconds: f64,
    pub sample_rate: u32,
    pub integrated_lufs: f32,
    pub true_peak_dbtp: f32,
    pub gain_offset_db: f32,
    pub pause_gap_seconds: f64,
    pub is_active: bool,
}

/// Everything needed to pick a project back up. Audio files are referenced by path.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    pub version: u32,
    pub track: Option<PathBuf>,
    pub reference: Option<PathBuf>,
    pub reference_gain: f32,
    /// Suite settings as an XML preset
    pub suite: String,
    /// Snapshot slots A-D as XML presets
    pub snapshots: [Option<String>; 4],
    pub active_snapshot: usize,
    pub trim: SessionTrim,
    pub loudness_matched_ab: bool,
    pub album_title: String,
    pub album_artist: String,
    pub target_album_lufs: f32,
    pub montage: Vec<SessionMontageTrack>,
    /// UI choices (export format, loudness target), stored as indices
    pub export_format: usize,
    pub loudness_target: usize,
}

/// UI-level choices that live outside the engine
#[derive(Clone, Copy, Debug, Default)]
pub struct SessionUiState {
    pub export_format: usize,
    pub loudness_target: usize,
}

impl Session {
    /// Capture the current project. `track` / `reference` are the files the user opened.
    pub fn capture(
        engine: &AudioEngine,
        snapshots: &Snapshots,
        track: Option<&Path>,
        reference: Option<&Path>,
        ui: SessionUiState,
    ) -> Self {
        let mut snaps: [Option<String>; 4] = [None, None, None, None];
        for (i, s) in snapshots.slots.iter().enumerate() {
            snaps[i] = s.as_ref().map(|p| p.to_xml_string());
        }
        let (album_title, album_artist, target_album_lufs, montage) = engine.with_montage(|m| {
            (
                m.album_title.clone(),
                m.album_artist.clone(),
                m.target_album_lufs,
                m.tracks
                    .iter()
                    .map(|t| SessionMontageTrack {
                        path: t.path.clone(),
                        title: t.title.clone(),
                        artist: t.artist.clone(),
                        duration_seconds: t.duration_seconds,
                        sample_rate: t.sample_rate,
                        integrated_lufs: t.integrated_lufs,
                        true_peak_dbtp: t.true_peak_dbtp,
                        gain_offset_db: t.gain_offset_db,
                        pause_gap_seconds: t.pause_gap_seconds,
                        is_active: t.is_active,
                    })
                    .collect(),
            )
        });
        Self {
            version: SESSION_VERSION,
            track: track.map(Path::to_path_buf),
            reference: reference.map(Path::to_path_buf),
            reference_gain: engine.reference_gain_offset(),
            suite: engine.suite_preset().to_xml_string(),
            snapshots: snaps,
            active_snapshot: snapshots.active,
            trim: SessionTrim::from_settings(&engine.trim_fade()),
            loudness_matched_ab: engine.ab_gain_match(),
            album_title,
            album_artist,
            target_album_lufs,
            montage,
            export_format: ui.export_format,
            loudness_target: ui.loudness_target,
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("Failed to write session: {}", e))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("Failed to read session: {}", e))?;
        let session: Session = serde_json::from_str(&text).map_err(|e| format!("Not a valid session file: {}", e))?;
        if session.version > SESSION_VERSION {
            return Err(format!("Session was saved by a newer version (format {})", session.version));
        }
        Ok(session)
    }

    /// Restore into the engine. Missing audio files are reported, not fatal.
    /// Returns warnings plus the UI-level choices to apply.
    pub fn restore(&self, engine: &AudioEngine, snapshots: &mut Snapshots) -> (Vec<String>, SessionUiState) {
        let mut warnings = Vec::new();
        engine.set_montage_mode(false);

        if let Some(ref path) = self.track {
            match load_audio_file(path) {
                Ok(buf) => engine.set_audio_buffer(buf),
                Err(e) => warnings.push(format!("Track {}: {}", path.display(), e)),
            }
        }
        if let Some(ref path) = self.reference {
            match load_audio_file(path) {
                Ok(buf) => {
                    engine.set_reference_buffer(buf);
                    engine.set_reference_gain_offset(self.reference_gain);
                }
                Err(e) => warnings.push(format!("Reference {}: {}", path.display(), e)),
            }
        }

        match XmlPreset::parse_str(&self.suite, "session".to_string()) {
            Ok(p) => engine.apply_preset(&p),
            Err(e) => warnings.push(format!("Suite settings: {}", e)),
        }
        snapshots.clear();
        for (i, s) in self.snapshots.iter().enumerate() {
            if let Some(xml) = s {
                if let Ok(p) = XmlPreset::parse_str(xml, format!("slot {}", SLOT_NAMES[i])) {
                    snapshots.slots[i] = Some(p);
                }
            }
        }
        snapshots.active = self.active_snapshot.min(3);

        engine.set_trim_fade(self.trim.to_settings());
        engine.set_ab_gain_match(self.loudness_matched_ab);

        engine.with_montage(|m| {
            m.album_title = self.album_title.clone();
            m.album_artist = self.album_artist.clone();
            m.target_album_lufs = self.target_album_lufs;
            m.tracks.clear();
            for t in &self.montage {
                if !t.path.exists() {
                    warnings.push(format!("Album track missing: {}", t.path.display()));
                }
                m.add_track(t.path.clone(), t.duration_seconds, t.sample_rate);
                if let Some(last) = m.tracks.last_mut() {
                    last.title = t.title.clone();
                    last.artist = t.artist.clone();
                    last.integrated_lufs = t.integrated_lufs;
                    last.true_peak_dbtp = t.true_peak_dbtp;
                    last.gain_offset_db = t.gain_offset_db;
                    last.pause_gap_seconds = t.pause_gap_seconds;
                    last.is_active = t.is_active;
                }
            }
        });

        (warnings, SessionUiState { export_format: self.export_format, loudness_target: self.loudness_target })
    }
}
