use std::path::{Path, PathBuf};
use crate::audio::buffer::AudioBuffer;
use crate::audio::decoder::load_audio_file;

pub const NUM_STEMS: usize = 6;

/// Distinct stem categories supported by the mastering workstation
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StemType {
    Drums,
    Bass,
    Vocals,
    Instruments,
    Synths,
    Ambience,
}

impl StemType {
    pub fn name(&self) -> &'static str {
        match self {
            StemType::Drums => "Drums & Percussion",
            StemType::Bass => "Bass & Sub",
            StemType::Vocals => "Vocals",
            StemType::Instruments => "Guitars & Keys",
            StemType::Synths => "Synths & FX",
            StemType::Ambience => "Ambience & Reverb",
        }
    }

    pub fn default_label(&self) -> &'static str {
        match self {
            StemType::Drums => "1. DRUMS",
            StemType::Bass => "2. BASS",
            StemType::Vocals => "3. VOCALS",
            StemType::Instruments => "4. GUITARS/KEYS",
            StemType::Synths => "5. SYNTHS/FX",
            StemType::Ambience => "6. AMBIENCE",
        }
    }

    pub fn color_bgr(&self) -> u32 {
        match self {
            StemType::Drums => 0x00A5FF,       // Gold/Amber
            StemType::Bass => 0xFF9900,        // Electric Blue
            StemType::Vocals => 0x00FF88,      // Neon Green
            StemType::Instruments => 0x3366FF, // Warm Coral
            StemType::Synths => 0xCC33FF,      // Bright Magenta/Violet
            StemType::Ambience => 0x00E5FF,    // Neon Cyan
        }
    }
}

/// Represents an individual audio stem within a multi-track stem mastering project
#[derive(Clone, Debug)]
pub struct AudioStem {
    pub id: usize,
    pub stem_type: StemType,
    pub name: String,
    pub file_path: Option<PathBuf>,
    pub buffer: Option<AudioBuffer>,
    pub gain_db: f32, // -24.0 dB to +12.0 dB
    pub pan: f32,     // -1.0 (L) to +1.0 (R)
    pub mute: bool,
    pub solo: bool,
}

impl AudioStem {
    pub fn new(id: usize, stem_type: StemType, name: &str) -> Self {
        Self {
            id,
            stem_type,
            name: name.to_string(),
            file_path: None,
            buffer: None,
            gain_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
        }
    }

    pub fn load_file(&mut self, path: PathBuf) -> Result<(), String> {
        let buf = load_audio_file(&path)?;
        self.name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        self.file_path = Some(path);
        self.buffer = Some(buf);
        Ok(())
    }

    pub fn load_buffer(&mut self, buffer: AudioBuffer, label: &str) {
        self.name = label.to_string();
        self.file_path = None;
        self.buffer = Some(buffer);
    }
}

/// 6-Channel Stem Mixer & Master Bus Summer
#[derive(Clone, Debug)]
pub struct StemMixer {
    pub is_enabled: bool,
    pub stems: [AudioStem; NUM_STEMS],
}

impl Default for StemMixer {
    fn default() -> Self {
        Self {
            is_enabled: false,
            stems: [
                AudioStem::new(1, StemType::Drums, "1. Drums"),
                AudioStem::new(2, StemType::Bass, "2. Bass"),
                AudioStem::new(3, StemType::Vocals, "3. Vocals"),
                AudioStem::new(4, StemType::Instruments, "4. Guitars & Keys"),
                AudioStem::new(5, StemType::Synths, "5. Synths & FX"),
                AudioStem::new(6, StemType::Ambience, "6. Ambience & Reverb"),
            ],
        }
    }
}

impl StemMixer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if at least one stem is currently loaded with audio
    pub fn has_loaded_stems(&self) -> bool {
        self.stems.iter().any(|s| s.buffer.is_some())
    }

    /// Set gain for a specific stem index (0 to NUM_STEMS - 1)
    pub fn set_stem_gain(&mut self, stem_idx: usize, gain_db: f32) {
        if stem_idx < NUM_STEMS {
            self.stems[stem_idx].gain_db = gain_db.clamp(-24.0, 12.0);
        }
    }

    /// Set pan for a specific stem index (0 to NUM_STEMS - 1)
    pub fn set_stem_pan(&mut self, stem_idx: usize, pan: f32) {
        if stem_idx < NUM_STEMS {
            self.stems[stem_idx].pan = pan.clamp(-1.0, 1.0);
        }
    }

    /// Toggle mute for a specific stem index
    pub fn toggle_stem_mute(&mut self, stem_idx: usize) {
        if stem_idx < NUM_STEMS {
            self.stems[stem_idx].mute = !self.stems[stem_idx].mute;
        }
    }

    /// Toggle solo for a specific stem index
    pub fn toggle_stem_solo(&mut self, stem_idx: usize) {
        if stem_idx < NUM_STEMS {
            self.stems[stem_idx].solo = !self.stems[stem_idx].solo;
        }
    }

    /// Reset and clear all loaded audio buffers and paths across all stems
    pub fn clear_all(&mut self) {
        for s in &mut self.stems {
            s.buffer = None;
            s.file_path = None;
            s.gain_db = 0.0;
            s.pan = 0.0;
            s.mute = false;
            s.solo = false;
        }
    }

    /// Export all loaded audio stems to 24-bit broadcast WAV files in destination folder
    pub fn export_all_stems<P: AsRef<Path>>(
        &self,
        out_dir: P,
        base_name: &str,
    ) -> Result<Vec<PathBuf>, String> {
        let dir = out_dir.as_ref();
        if !dir.exists() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("Failed to create export directory: {}", e))?;
        }

        let mut exported_files = Vec::new();

        for (idx, stem) in self.stems.iter().enumerate() {
            if let Some(ref buf) = stem.buffer {
                let safe_stem_name = stem.stem_type.name().replace(" & ", "_").replace(" ", "_");
                let out_file = dir.join(format!("{}_{}_{}.wav", base_name, idx + 1, safe_stem_name));

                let spec = hound::WavSpec {
                    channels: 2,
                    sample_rate: buf.sample_rate,
                    bits_per_sample: 24,
                    sample_format: hound::SampleFormat::Int,
                };

                let mut writer = hound::WavWriter::create(&out_file, spec)
                    .map_err(|e| format!("Failed to create stem WAV {}: {}", out_file.display(), e))?;

                let (left, right) = buf.to_stereo();
                for i in 0..left.len() {
                    let s_l = (left[i].clamp(-1.0, 1.0) * 8388607.0) as i32;
                    let s_r = (right[i].clamp(-1.0, 1.0) * 8388607.0) as i32;
                    writer.write_sample(s_l).map_err(|e| e.to_string())?;
                    writer.write_sample(s_r).map_err(|e| e.to_string())?;
                }

                writer.finalize().map_err(|e| e.to_string())?;
                exported_files.push(out_file);
            }
        }

        if exported_files.is_empty() {
            return Err("No active stems loaded to export".to_string());
        }

        Ok(exported_files)
    }

    /// Sum all active stems into a unified master stereo AudioBuffer with constant-power panning
    pub fn sum_to_master_buffer(&self) -> Result<AudioBuffer, String> {
        let loaded_stems: Vec<&AudioStem> = self
            .stems
            .iter()
            .filter(|s| s.buffer.is_some())
            .collect();

        if loaded_stems.is_empty() {
            return Err("No stems loaded to sum".to_string());
        }

        let sample_rate = loaded_stems[0].buffer.as_ref().unwrap().sample_rate;
        let max_frames = loaded_stems
            .iter()
            .map(|s| s.buffer.as_ref().unwrap().num_frames())
            .max()
            .unwrap_or(0);

        if max_frames == 0 {
            return Err("Loaded stems contain 0 audio frames".to_string());
        }

        let has_any_solo = self.stems.iter().any(|s| s.solo && s.buffer.is_some());

        let mut sum_left = vec![0.0f32; max_frames];
        let mut sum_right = vec![0.0f32; max_frames];

        for stem in &self.stems {
            if let Some(ref buf) = stem.buffer {
                // Solo check
                if has_any_solo && !stem.solo {
                    continue;
                }
                // Mute check
                if stem.mute && !stem.solo {
                    continue;
                }

                let gain_linear = 10.0f32.powf(stem.gain_db / 20.0);

                // Constant power panning law
                let pan_angle = (stem.pan + 1.0) * (std::f32::consts::PI / 4.0); // 0 to pi/2
                let pan_l = pan_angle.cos() * std::f32::consts::SQRT_2;
                let pan_r = pan_angle.sin() * std::f32::consts::SQRT_2;

                let left_gain = gain_linear * pan_l;
                let right_gain = gain_linear * pan_r;

                let (s_left, s_right) = buf.to_stereo();
                let n = s_left.len();

                for i in 0..n {
                    sum_left[i] += s_left[i] * left_gain;
                    sum_right[i] += s_right[i] * right_gain;
                }
            }
        }

        Ok(AudioBuffer::from_stereo(sum_left, sum_right, sample_rate))
    }
}
