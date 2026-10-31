//! Production-style embeddings with the bundled AFx-Rep model.
//!
//! The model turns a clip into a 512-dim mid and a 512-dim side embedding that track EQ, dynamics,
//! width and saturation rather than musical content, so two clips can be scored on how close they
//! sound. It never processes audio. The ONNX export takes a power spectrogram: the STFT and the
//! reference code's peak normalisation happen here, while the mel filterbank and log scaling stay
//! in the graph with the checkpoint's own values.
//!
//! Peak normalisation makes the embedding independent of the clip's gain.
//!
//! Model: AFx-Rep from ST-ITO by Steinmetz et al. (Apache 2.0), see models/README.md.

use std::f32::consts::PI;
use std::path::{Path, PathBuf};

use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;

use super::ml_separator::{find_model_file, reflect_pad};
use super::resample::SincResampler;
use super::separator::Radix2Fft;

pub const MODEL_FILE: &str = "afx_rep.onnx";
pub const MODEL_SAMPLE_RATE: u32 = 48000;
pub const EMBED_DIM: usize = 512;
/// Weight of the side embedding in `StyleEmbedding::distance`
pub const SIDE_WEIGHT: f32 = 0.5;

const NFFT: usize = 2048;
const HOP: usize = 1024;
const BINS: usize = NFFT / 2 + 1;
/// The network halves the time axis five times
const MIN_FRAMES: usize = 32;
/// Clips sent through the model at once
const MAX_BATCH: usize = 12;
/// Shortest audio worth comparing, comfortably above `MIN_FRAMES`
pub const MIN_SECONDS: f64 = 1.0;

/// Look for the model the way the stem model is found
pub fn find_model() -> Option<PathBuf> {
    find_model_file(MODEL_FILE)
}

/// Unit-length mid and side embeddings of a clip
#[derive(Clone, Debug)]
pub struct StyleEmbedding {
    pub mid: [f32; EMBED_DIM],
    pub side: [f32; EMBED_DIM],
}

impl StyleEmbedding {
    fn new(mid: &[f32], side: &[f32]) -> Self {
        let mut out = Self { mid: [0.0; EMBED_DIM], side: [0.0; EMBED_DIM] };
        out.mid.copy_from_slice(mid);
        out.side.copy_from_slice(side);
        normalize(&mut out.mid);
        normalize(&mut out.side);
        out
    }

    /// 0 for the same style, growing as two clips sound less alike
    pub fn distance(&self, other: &StyleEmbedding) -> f32 {
        (1.0 - cosine(&self.mid, &other.mid)) + SIDE_WEIGHT * (1.0 - cosine(&self.side, &other.side))
    }

    /// The style of several clips taken together
    pub fn average(items: &[StyleEmbedding]) -> Option<StyleEmbedding> {
        if items.is_empty() {
            return None;
        }
        let mut mean = StyleEmbedding { mid: [0.0; EMBED_DIM], side: [0.0; EMBED_DIM] };
        for item in items {
            for i in 0..EMBED_DIM {
                mean.mid[i] += item.mid[i];
                mean.side[i] += item.side[i];
            }
        }
        normalize(&mut mean.mid);
        normalize(&mut mean.side);
        Some(mean)
    }
}

fn normalize(v: &mut [f32; EMBED_DIM]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    for x in v.iter_mut() {
        *x /= norm;
    }
}

fn cosine(a: &[f32; EMBED_DIM], b: &[f32; EMBED_DIM]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// A clip's model input: mid then side power spectrograms, `frames` x `BINS` each
pub struct StyleFeatures {
    frames: usize,
    power: Vec<f32>,
}

/// Everything that happens to a clip before the model. It has no session, so render threads can
/// share it.
pub struct StyleFrontEnd {
    fft: Radix2Fft,
    window: Vec<f32>,
    resampler: SincResampler,
}

impl Default for StyleFrontEnd {
    fn default() -> Self {
        // Periodic Hann, as the checkpoint's STFT kernels
        let window = (0..NFFT).map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / NFFT as f32).cos())).collect();
        Self { fft: Radix2Fft::new(NFFT), window, resampler: SincResampler::default() }
    }
}

impl StyleFrontEnd {
    /// Resample to the model's rate, peak-normalise, split into mid and side, and take the STFT
    pub fn features(&self, left: &[f32], right: &[f32], sample_rate: u32) -> Result<StyleFeatures, String> {
        let (l, r) = self.resampler.resample_stereo(left, right, sample_rate, MODEL_SAMPLE_RATE);
        let n = l.len().min(r.len());
        let frames = 1 + n / HOP;
        if frames < MIN_FRAMES {
            return Err("Clip is too short to compare".to_string());
        }
        // A candidate's settings can blow up; that clip then just scores badly
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        let peak = l[..n].iter().chain(&r[..n]).fold(0.0f32, |p, &v| p.max(finite(v).abs())).max(1e-8);
        let mid: Vec<f32> = (0..n).map(|i| (finite(l[i]) + finite(r[i])) * 0.5 / peak).collect();
        let side: Vec<f32> = (0..n).map(|i| (finite(l[i]) - finite(r[i])) * 0.5 / peak).collect();

        let mut power = vec![0.0f32; 2 * frames * BINS];
        let (mid_power, side_power) = power.split_at_mut(frames * BINS);
        self.power_spectrogram(&mid, mid_power, frames);
        self.power_spectrogram(&side, side_power, frames);
        Ok(StyleFeatures { frames, power })
    }

    /// Centred STFT with reflect padding; `out` is `frames` x `BINS`
    fn power_spectrogram(&self, x: &[f32], out: &mut [f32], frames: usize) {
        let padded = reflect_pad(x, NFFT / 2, NFFT / 2);
        let mut re = vec![0.0f32; NFFT];
        let mut im = vec![0.0f32; NFFT];
        for (t, row) in out.chunks_exact_mut(BINS).take(frames).enumerate() {
            let start = t * HOP;
            for i in 0..NFFT {
                re[i] = padded[start + i] * self.window[i];
            }
            im.fill(0.0);
            self.fft.forward(&mut re, &mut im);
            for (k, p) in row.iter_mut().enumerate() {
                *p = re[k] * re[k] + im[k] * im[k];
            }
        }
    }
}

pub struct StyleModel {
    session: Session,
    front_end: StyleFrontEnd,
}

impl StyleModel {
    pub fn load(path: &Path) -> Result<Self, String> {
        // About the physical cores: more only contend, and the UI and audio threads need room
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        let session = Session::builder()
            .and_then(|b| b.with_optimization_level(GraphOptimizationLevel::Level3))
            .and_then(|b| b.with_intra_threads((threads / 2).max(1)))
            .and_then(|b| b.commit_from_file(path))
            .map_err(|e| format!("Failed to load style model {}: {}", path.display(), e))?;
        Ok(Self { session, front_end: StyleFrontEnd::default() })
    }

    /// Load the bundled model if it can be found
    pub fn load_default() -> Result<Self, String> {
        let path = find_model().ok_or_else(|| format!("Style model models/{} not found", MODEL_FILE))?;
        Self::load(&path)
    }

    pub fn front_end(&self) -> &StyleFrontEnd {
        &self.front_end
    }

    /// Style of one stereo clip of at least about 0.7 s
    pub fn embed(&mut self, left: &[f32], right: &[f32], sample_rate: u32) -> Result<StyleEmbedding, String> {
        let features = self.front_end.features(left, right, sample_rate)?;
        let mut out = self.embed_features(std::slice::from_ref(&features))?;
        out.pop().ok_or_else(|| "Style model returned nothing".to_string())
    }

    /// Style of clips already through `StyleFrontEnd::features`, batched where lengths match
    pub fn embed_features(&mut self, features: &[StyleFeatures]) -> Result<Vec<StyleEmbedding>, String> {
        let mut out = Vec::with_capacity(features.len());
        for same_length in features.chunk_by(|a, b| a.frames == b.frames) {
            for batch in same_length.chunks(MAX_BATCH) {
                self.run_batch(batch, &mut out)?;
            }
        }
        Ok(out)
    }

    fn run_batch(&mut self, batch: &[StyleFeatures], out: &mut Vec<StyleEmbedding>) -> Result<(), String> {
        let frames = batch[0].frames;
        let mut power = Vec::with_capacity(batch.len() * 2 * frames * BINS);
        for item in batch {
            power.extend_from_slice(&item.power);
        }
        let input = Tensor::from_array(([batch.len() * 2, 1, frames, BINS], power)).map_err(|e| e.to_string())?;
        let outputs = self
            .session
            .run(ort::inputs!["power" => input])
            .map_err(|e| format!("Style model inference failed: {}", e))?;
        let (_, mid) = outputs["mid"].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        let (_, side) = outputs["side"].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        if mid.len() != batch.len() * EMBED_DIM || side.len() != mid.len() {
            return Err("Style model returned unexpected tensor sizes".to_string());
        }
        out.extend(mid.chunks_exact(EMBED_DIM).zip(side.chunks_exact(EMBED_DIM)).map(|(m, s)| StyleEmbedding::new(m, s)));
        Ok(())
    }
}
