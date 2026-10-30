//! Trained stem separation with the bundled HTDemucs (Hybrid Transformer Demucs v4) model.
//!
//! The ONNX export takes the waveform plus its complex spectrogram and returns the model's two
//! branches (spectral and waveform) per source, so the STFT, iSTFT and branch sum happen here.
//! The pre/post-processing mirrors the reference implementation: whole-track normalisation,
//! 7.8 s segments with 25% overlap blended by a triangular window, and HTDemucs' padded STFT
//! (n_fft 4096, hop 1024, periodic Hann, normalised, reflect padding).
//!
//! Model: HTDemucs by Meta (MIT licence), ONNX conversion from huggingface.co/Ghilda/htdemucs-onnx.

use std::f32::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;

use super::buffer::AudioBuffer;
use super::resample::SincResampler;
use super::separator::Radix2Fft;

pub const MODEL_FILE: &str = "htdemucs.onnx";
pub const MODEL_SAMPLE_RATE: u32 = 44100;
/// Source order of the model's outputs
pub const SOURCE_NAMES: [&str; 4] = ["Drums", "Bass", "Other", "Vocals"];

const SEGMENT: usize = 343_980;
const NFFT: usize = 4096;
const HOP: usize = 1024;
const BINS: usize = 2048;
const FRAMES: usize = 336;
const OVERLAP: f32 = 0.25;

/// Four separated stems (drums, bass, other, vocals) aligned with the buffer they came from
#[derive(Debug)]
pub struct StemSet {
    pub stems: [AudioBuffer; 4],
    pub sample_rate: u32,
    pub num_frames: usize,
}

impl StemSet {
    /// Interpolated stereo frame of each stem at a fractional source position
    #[inline]
    pub fn frames_at(&self, pos: f64) -> [(f32, f32); 4] {
        [0, 1, 2, 3].map(|s| self.stems[s].get_interpolated_frame(pos))
    }

    #[inline]
    pub fn frames_at_index(&self, frame: usize) -> [(f32, f32); 4] {
        [0, 1, 2, 3].map(|s| self.stems[s].get_stereo_frame(frame))
    }
}

/// Look for the model in `models/` under the working directory, next to the executable, or up to
/// four parent folders of it (so `target\release\mastering_studio.exe` finds the repo copy)
pub fn find_model() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from("models").join(MODEL_FILE)];
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1).take(5) {
            candidates.push(dir.join("models").join(MODEL_FILE));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

pub struct DemucsSeparator {
    session: Session,
    stft: StftCore,
}

/// HTDemucs' STFT/iSTFT (kept apart from the session so model outputs can be borrowed meanwhile)
struct StftCore {
    fft: Radix2Fft,
    window: Vec<f32>,
}

impl DemucsSeparator {
    pub fn load(path: &Path) -> Result<Self, String> {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let session = Session::builder()
            .and_then(|b| b.with_optimization_level(GraphOptimizationLevel::Level3))
            .and_then(|b| b.with_intra_threads(threads))
            .and_then(|b| b.commit_from_file(path))
            .map_err(|e| format!("Failed to load stem model {}: {}", path.display(), e))?;
        // Periodic Hann, as torch.hann_window
        let window = (0..NFFT).map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / NFFT as f32).cos())).collect();
        Ok(Self { session, stft: StftCore { fft: Radix2Fft::new(NFFT), window } })
    }

    /// Load the bundled model if it can be found
    pub fn load_default() -> Result<Self, String> {
        let path = find_model().ok_or_else(|| format!("Stem model models/{} not found", MODEL_FILE))?;
        Self::load(&path)
    }

    /// Separate a track into drums, bass, other and vocals at the track's own rate and length
    pub fn separate(&mut self, input: &AudioBuffer, progress: impl Fn(f32)) -> Result<StemSet, String> {
        let num_frames = input.num_frames();
        if num_frames == 0 {
            return Err("Track is empty".to_string());
        }
        let resampler = SincResampler::default();
        let at_model_rate = if input.sample_rate != MODEL_SAMPLE_RATE {
            resampler.resample_buffer(input, MODEL_SAMPLE_RATE)
        } else {
            input.clone()
        };
        let (mut l, mut r) = at_model_rate.to_stereo();
        let n = l.len();

        // Whole-track normalisation from the mono reference
        let mono: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
        let mean = mono.iter().map(|&v| v as f64).sum::<f64>() / n as f64;
        let var = mono.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / (n.max(2) - 1) as f64;
        let (mean, std) = (mean as f32, (var.sqrt() as f32).max(1e-8));
        for v in l.iter_mut().chain(r.iter_mut()) {
            *v = (*v - mean) / std;
        }

        // Triangular blend window, normalised to a peak of 1
        let half = SEGMENT / 2;
        let weight: Vec<f32> = (0..SEGMENT)
            .map(|i| if i < half { (i + 1) as f32 } else { (SEGMENT - i) as f32 })
            .map(|w| w / half as f32)
            .collect();
        let stride = ((1.0 - OVERLAP) * SEGMENT as f32) as usize;
        let offsets: Vec<usize> = (0..n).step_by(stride).collect();

        let mut out = vec![vec![0.0f32; n]; 8]; // source * 2 + channel
        let mut weight_sum = vec![0.0f32; n];

        for (seg_idx, &offset) in offsets.iter().enumerate() {
            progress(seg_idx as f32 / offsets.len() as f32);
            let chunk_len = SEGMENT.min(n - offset);
            // Short chunks are padded symmetrically with surrounding audio (zeros past the ends)
            let left_pad = (SEGMENT - chunk_len) / 2;
            let start = offset as isize - left_pad as isize;
            let read = |src: &[f32]| -> Vec<f32> {
                (0..SEGMENT)
                    .map(|i| {
                        let j = start + i as isize;
                        if j >= 0 && (j as usize) < src.len() { src[j as usize] } else { 0.0 }
                    })
                    .collect()
            };
            let seg_l = read(&l);
            let seg_r = read(&r);

            let sources = self.run_segment(&seg_l, &seg_r)?;
            for (k, src) in sources.iter().enumerate() {
                let dst = &mut out[k];
                for i in 0..chunk_len {
                    dst[offset + i] += src[left_pad + i] * weight[i];
                }
            }
            for i in 0..chunk_len {
                weight_sum[offset + i] += weight[i];
            }
        }
        progress(1.0);

        // Undo the normalisation. The reference adds the mean back to every source, which leaves
        // the stems' sum off by three times any DC offset; add it once, to "other", instead.
        for (k, ch) in out.iter_mut().enumerate() {
            let dc = if k / 2 == 2 { mean } else { 0.0 };
            for (v, w) in ch.iter_mut().zip(&weight_sum) {
                *v = *v / w.max(1e-8) * std + dc;
            }
        }

        let mut stems = Vec::with_capacity(4);
        for s in 0..4 {
            let sl = std::mem::take(&mut out[s * 2]);
            let sr = std::mem::take(&mut out[s * 2 + 1]);
            let mut buf = AudioBuffer::from_stereo(sl, sr, MODEL_SAMPLE_RATE);
            if input.sample_rate != MODEL_SAMPLE_RATE {
                buf = resampler.resample_buffer(&buf, input.sample_rate);
            }
            // Resampling can be off by a sample; pin every stem to the source length
            let (mut a, mut b) = buf.to_stereo();
            a.resize(num_frames, 0.0);
            b.resize(num_frames, 0.0);
            stems.push(AudioBuffer::from_stereo(a, b, input.sample_rate));
        }
        let stems: [AudioBuffer; 4] = stems.try_into().map_err(|_| "internal: stem count".to_string())?;
        Ok(StemSet { stems, sample_rate: input.sample_rate, num_frames })
    }

    /// Run the model on one normalised segment; returns 8 channels (source * 2 + channel)
    fn run_segment(&mut self, seg_l: &[f32], seg_r: &[f32]) -> Result<Vec<Vec<f32>>, String> {
        // Complex-as-channels spectrogram: [L.re, L.im, R.re, R.im] x BINS x FRAMES
        let mut mag = vec![0.0f32; 4 * BINS * FRAMES];
        for (c, seg) in [seg_l, seg_r].into_iter().enumerate() {
            let (re, im) = self.stft.spec(seg);
            let base_re = (c * 2) * BINS * FRAMES;
            let base_im = (c * 2 + 1) * BINS * FRAMES;
            mag[base_re..base_re + BINS * FRAMES].copy_from_slice(&re);
            mag[base_im..base_im + BINS * FRAMES].copy_from_slice(&im);
        }
        let mut mix = Vec::with_capacity(2 * SEGMENT);
        mix.extend_from_slice(seg_l);
        mix.extend_from_slice(seg_r);

        let mix_t = Tensor::from_array(([1usize, 2, SEGMENT], mix)).map_err(|e| e.to_string())?;
        let mag_t = Tensor::from_array(([1usize, 4, BINS, FRAMES], mag)).map_err(|e| e.to_string())?;
        let outputs = self
            .session
            .run(ort::inputs!["mix" => mix_t, "mag" => mag_t])
            .map_err(|e| format!("Stem model inference failed: {}", e))?;
        let (_, freq) = outputs["freq"].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        let (_, time) = outputs["time"].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        if freq.len() != 4 * 4 * BINS * FRAMES || time.len() != 4 * 2 * SEGMENT {
            return Err("Stem model returned unexpected tensor sizes".to_string());
        }

        let plane = BINS * FRAMES;
        let mut result = Vec::with_capacity(8);
        for s in 0..4 {
            for c in 0..2 {
                let base = (s * 4 + c * 2) * plane;
                let re = &freq[base..base + plane];
                let im = &freq[base + plane..base + 2 * plane];
                let mut wave = self.stft.ispec(re, im);
                let t = &time[(s * 2 + c) * SEGMENT..(s * 2 + c + 1) * SEGMENT];
                for (w, tv) in wave.iter_mut().zip(t) {
                    *w += *tv;
                }
                result.push(wave);
            }
        }
        Ok(result)
    }
}

impl StftCore {
    /// HTDemucs `_spec`: reflect-pad so frames align with the hop, centred normalised STFT,
    /// drop the Nyquist bin and the two edge frames on each side. Returns BINS x FRAMES (re, im).
    fn spec(&self, x: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let len = x.len();
        let le = (len + HOP - 1) / HOP;
        let pad = HOP / 2 * 3;
        let padded = reflect_pad(x, pad, pad + le * HOP - len);
        let centred = reflect_pad(&padded, NFFT / 2, NFFT / 2);
        let scale = 1.0 / (NFFT as f32).sqrt();

        let mut out_re = vec![0.0f32; BINS * FRAMES];
        let mut out_im = vec![0.0f32; BINS * FRAMES];
        let mut re = vec![0.0f32; NFFT];
        let mut im = vec![0.0f32; NFFT];
        for t in 0..le.min(FRAMES) {
            let frame = t + 2;
            let start = frame * HOP;
            for i in 0..NFFT {
                re[i] = centred[start + i] * self.window[i];
            }
            im.fill(0.0);
            self.fft.forward(&mut re, &mut im);
            for k in 0..BINS {
                out_re[k * FRAMES + t] = re[k] * scale;
                out_im[k * FRAMES + t] = im[k] * scale;
            }
        }
        (out_re, out_im)
    }

    /// HTDemucs `_ispec`: restore the Nyquist bin and edge frames, centred normalised iSTFT with
    /// window-square normalisation, then trim the padding back off. Returns SEGMENT samples.
    fn ispec(&self, z_re: &[f32], z_im: &[f32]) -> Vec<f32> {
        let frames = FRAMES + 4;
        let pad = HOP / 2 * 3;
        let le = HOP * ((SEGMENT + HOP - 1) / HOP) + 2 * pad;
        let total = NFFT + HOP * (frames - 1);
        let mut acc = vec![0.0f32; total];
        let mut env = vec![0.0f32; total];
        let scale = (NFFT as f32).sqrt();

        let mut re = vec![0.0f32; NFFT];
        let mut im = vec![0.0f32; NFFT];
        for frame in 0..frames {
            re.fill(0.0);
            im.fill(0.0);
            if (2..2 + FRAMES).contains(&frame) {
                let t = frame - 2;
                for k in 0..BINS {
                    re[k] = z_re[k * FRAMES + t];
                    im[k] = z_im[k * FRAMES + t];
                }
                // Hermitian mirror (Nyquist bin stays zero)
                for k in 1..BINS {
                    re[NFFT - k] = re[k];
                    im[NFFT - k] = -im[k];
                }
                im[0] = 0.0;
                self.fft.inverse(&mut re, &mut im);
            }
            let start = frame * HOP;
            for i in 0..NFFT {
                let w = self.window[i];
                acc[start + i] += re[i] * scale * w;
                env[start + i] += w * w;
            }
        }
        // Undo the centre padding, then the HTDemucs padding
        let offset = NFFT / 2 + pad;
        (0..SEGMENT)
            .map(|i| {
                let j = offset + i;
                if j < total && i + pad < le { acc[j] / env[j].max(1e-11) } else { 0.0 }
            })
            .collect()
    }
}

/// torch-style reflect padding (the edge sample is not repeated)
fn reflect_pad(x: &[f32], left: usize, right: usize) -> Vec<f32> {
    let n = x.len();
    let mut out = Vec::with_capacity(n + left + right);
    let reflect = |i: isize| -> f32 {
        if n < 2 {
            return x.first().copied().unwrap_or(0.0);
        }
        let period = 2 * (n as isize - 1);
        let mut j = i.rem_euclid(period);
        if j >= n as isize {
            j = period - j;
        }
        x[j as usize]
    };
    for i in (1..=left as isize).rev() {
        out.push(reflect(-i));
    }
    out.extend_from_slice(x);
    for i in 0..right as isize {
        out.push(reflect(n as isize + i));
    }
    out
}

/// Real-time stem bus: per-stem gain (Stem Rebalance) and 3-band EQ (Stem EQ) applied to
/// separated stems, then summed. Used by playback and offline export when trained stems exist.
#[derive(Clone, Debug)]
pub struct StemBus {
    filters: [[crate::dsp::BiquadFilter; 3]; 4],
    eq_db: [[f32; 3]; 4],
    gains: [f32; 4],
    eq_on: bool,
    sample_rate: f32,
}

/// Rebalance/EQ order is vocals, bass, drums, other; the model's is drums, bass, other, vocals
const MODEL_TO_STEM_PARAM: [usize; 4] = [2, 1, 3, 0];

impl StemBus {
    pub fn new(sample_rate: f32) -> Self {
        use crate::dsp::{BiquadFilter, FilterType};
        let make = || {
            [
                BiquadFilter::new(FilterType::LowShelf, 250.0, 0.7, 0.0, sample_rate),
                BiquadFilter::new(FilterType::Peaking, 1500.0, 0.5, 0.0, sample_rate),
                BiquadFilter::new(FilterType::HighShelf, 5000.0, 0.7, 0.0, sample_rate),
            ]
        };
        Self { filters: [make(), make(), make(), make()], eq_db: [[0.0; 3]; 4], gains: [1.0; 4], eq_on: false, sample_rate }
    }

    pub fn reset(&mut self) {
        for f in self.filters.iter_mut().flatten() {
            f.reset_state();
        }
    }

    /// Pull current Stem Rebalance / Stem EQ settings from the suite
    pub fn configure(&mut self, suite: &crate::dsp::MasteringSuite) {
        if (suite.sample_rate - self.sample_rate).abs() > 0.5 {
            self.sample_rate = suite.sample_rate;
            for f in self.filters.iter_mut().flatten() {
                f.set_sample_rate(self.sample_rate);
            }
        }
        let rebalance_on = suite.stem_rebalance.enabled;
        self.eq_on = suite.stem_eq.enabled;
        for (model_idx, &param_idx) in MODEL_TO_STEM_PARAM.iter().enumerate() {
            let db = if rebalance_on { suite.stem_rebalance.gains_db[param_idx] } else { 0.0 };
            self.gains[model_idx] = 10.0f32.powf(db / 20.0);
            let bands = suite.stem_eq.bands_db[param_idx];
            if bands != self.eq_db[model_idx] {
                self.eq_db[model_idx] = bands;
                for (b, f) in self.filters[model_idx].iter_mut().enumerate() {
                    let (freq, q) = [(250.0, 0.7), (1500.0, 0.5), (5000.0, 0.7)][b];
                    f.set_params(freq, q, bands[b]);
                }
            }
        }
    }

    #[inline]
    pub fn mix(&mut self, frames: [(f32, f32); 4]) -> (f32, f32) {
        let (mut l, mut r) = (0.0, 0.0);
        for (s, (mut a, mut b)) in frames.into_iter().enumerate() {
            if self.eq_on {
                for f in self.filters[s].iter_mut() {
                    (a, b) = f.process_sample(a, b);
                }
            }
            l += a * self.gains[s];
            r += b * self.gains[s];
        }
        (l, r)
    }
}

/// Convenience for background jobs: load the bundled model and separate
pub fn separate_with_bundled_model(input: &AudioBuffer, progress: impl Fn(f32)) -> Result<Arc<StemSet>, String> {
    let mut sep = DemucsSeparator::load_default()?;
    sep.separate(input, progress).map(Arc::new)
}
