//! Codec preview: the master as it will sound after lossy encoding. The rendered master is
//! encoded with the export codec, decoded again, aligned to the master (encoders add delay),
//! and kept with its difference to the master, i.e. what the codec throws away.

use super::buffer::AudioBuffer;
use super::decoder::load_audio_file;
use super::encoder::ExportCodec;
use super::qc::{analyze_buffer, QcTargets};
use super::renderer::encode_buffer;
use super::resample::SincResampler;

/// Largest encoder delay searched for when aligning the decoded audio
const MAX_CODEC_DELAY: usize = 8192;
/// Length of the excerpt the alignment correlates
const ALIGN_WINDOW: usize = 1 << 15;

/// What the transport plays while a codec preview is loaded
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PreviewMode {
    /// The live processed signal (normal playback)
    #[default]
    Off,
    /// The master after encoding and decoding
    Codec,
    /// Master minus codec: the artefacts and the detail the codec drops
    Difference,
}

impl PreviewMode {
    pub fn next(self) -> Self {
        match self {
            PreviewMode::Off => PreviewMode::Codec,
            PreviewMode::Codec => PreviewMode::Difference,
            PreviewMode::Difference => PreviewMode::Off,
        }
    }
}

#[derive(Debug)]
pub struct CodecPreview {
    pub codec: ExportCodec,
    /// Decoded audio, aligned with the track (same rate and length as the master)
    pub decoded: AudioBuffer,
    /// Master minus decoded
    pub difference: AudioBuffer,
    /// Samples of delay the encoder added (removed in `decoded`)
    pub encoder_delay: usize,
    pub master_true_peak_dbtp: f32,
    pub codec_true_peak_dbtp: f32,
    pub master_lufs: f32,
    pub codec_lufs: f32,
    /// Level of the difference relative to the master (dB, lower is more transparent)
    pub difference_db: f32,
}

impl CodecPreview {
    /// Encode `master` with `codec` and decode it again. `master` is the rendered, unquantized
    /// master at the track's rate.
    pub fn build(master: &AudioBuffer, codec: ExportCodec, progress: impl Fn(f32)) -> Result<Self, String> {
        if !codec.is_lossy() {
            return Err(format!("{} is lossless: there is nothing to preview", codec.name()));
        }
        let path = std::env::temp_dir().join(format!("mastering_studio_preview_{}.{}", std::process::id(), codec.extension()));
        encode_buffer(master, &path, codec, None)?;
        progress(0.4);
        let decoded = load_audio_file(&path);
        let _ = std::fs::remove_file(&path);
        let mut decoded = decoded?;
        progress(0.6);
        if decoded.sample_rate != master.sample_rate {
            decoded = SincResampler::default().resample_buffer(&decoded, master.sample_rate);
        }

        let (ml, mr) = master.to_stereo();
        let (dl, dr) = decoded.to_stereo();
        let delay = find_delay(&ml, &mr, &dl, &dr, MAX_CODEC_DELAY);
        let n = ml.len();
        let take = |d: &[f32]| -> Vec<f32> { (0..n).map(|i| d.get(i + delay).copied().unwrap_or(0.0)).collect() };
        let (al, ar) = (take(&dl), take(&dr));
        let diff_l: Vec<f32> = ml.iter().zip(&al).map(|(m, c)| m - c).collect();
        let diff_r: Vec<f32> = mr.iter().zip(&ar).map(|(m, c)| m - c).collect();
        progress(0.75);

        let energy = |a: &[f32], b: &[f32]| a.iter().chain(b).map(|v| (*v as f64).powi(2)).sum::<f64>();
        let difference_db = (10.0 * (energy(&diff_l, &diff_r).max(1e-20) / energy(&ml, &mr).max(1e-20)).log10()) as f32;

        let decoded = AudioBuffer::from_stereo(al, ar, master.sample_rate);
        let difference = AudioBuffer::from_stereo(diff_l, diff_r, master.sample_rate);
        let qc_master = analyze_buffer(master, "master", "master", QcTargets::default());
        progress(0.9);
        let qc_codec = analyze_buffer(&decoded, "codec", codec.name(), QcTargets::default());
        Ok(Self {
            codec,
            decoded,
            difference,
            encoder_delay: delay,
            master_true_peak_dbtp: qc_master.true_peak_dbtp[0].max(qc_master.true_peak_dbtp[1]),
            codec_true_peak_dbtp: qc_codec.true_peak_dbtp[0].max(qc_codec.true_peak_dbtp[1]),
            master_lufs: qc_master.integrated_lufs,
            codec_lufs: qc_codec.integrated_lufs,
            difference_db,
        })
    }

    /// One-line summary for the transport
    pub fn summary(&self) -> String {
        let label = match self.codec {
            ExportCodec::Mp3(k) => format!("MP3 {} kbps", k),
            ExportCodec::Aac(k) => format!("AAC {} kbps", k),
            other => other.name().to_string(),
        };
        format!(
            "{}: true peak {:.1} dBTP ({:+.1} dB vs master), difference {:.0} dB below the master",
            label,
            self.codec_true_peak_dbtp,
            self.codec_true_peak_dbtp - self.master_true_peak_dbtp,
            -self.difference_db
        )
    }
}

/// Delay (0..=max) at which the decoded signal best matches the master, by cross-correlating a
/// loud excerpt of the mid signal
fn find_delay(ml: &[f32], mr: &[f32], dl: &[f32], dr: &[f32], max: usize) -> usize {
    let n = ml.len().min(dl.len().saturating_sub(max));
    if n < 1024 {
        return 0;
    }
    let len = ALIGN_WINDOW.min(n);
    // Loudest stretch, so the correlation is not taken over silence
    let step = (len / 4).max(1);
    let start = (0..=n - len)
        .step_by(step)
        .max_by(|&a, &b| {
            let e = |s: usize| (s..s + len).step_by(16).map(|i| (ml[i] + mr[i]).powi(2)).sum::<f32>();
            e(a).partial_cmp(&e(b)).unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(0);
    let master: Vec<f32> = (start..start + len).map(|i| ml[i] + mr[i]).collect();
    let decoded: Vec<f32> = (start..(start + len + max).min(dl.len())).map(|i| dl[i] + dr[i]).collect();
    (0..=max)
        .filter(|lag| lag + len <= decoded.len())
        .map(|lag| (lag, master.iter().zip(&decoded[lag..]).map(|(a, b)| a * b).sum::<f32>()))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map_or(0, |(lag, _)| lag)
}
