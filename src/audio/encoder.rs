//! Export formats: WAV, FLAC, MP3 and AAC. The renderer streams the finished master into a sink
//! that dithers and encodes it.

use std::path::{Path, PathBuf};

use super::bwf::BwfMetadata;
use super::media_foundation::{LossyCodec, MfEncoder};
use super::renderer::ExportBitDepth;
use crate::dsp::{DitherType, MasteringDither};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExportCodec {
    Wav(ExportBitDepth),
    /// Lossless; 16 or 24 bit (32-bit float is stored as 24-bit)
    Flac(ExportBitDepth),
    /// Windows' built-in MP3 encoder at the given bit rate (kbit/s)
    Mp3(u32),
    /// Windows' built-in AAC-LC encoder, in an .m4a file, at the given bit rate (kbit/s)
    Aac(u32),
}

impl ExportCodec {
    pub fn extension(self) -> &'static str {
        match self {
            ExportCodec::Wav(_) => "wav",
            ExportCodec::Flac(_) => "flac",
            ExportCodec::Mp3(_) => "mp3",
            ExportCodec::Aac(_) => "m4a",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ExportCodec::Wav(_) => "WAV",
            ExportCodec::Flac(_) => "FLAC",
            ExportCodec::Mp3(_) => "MP3",
            ExportCodec::Aac(_) => "AAC",
        }
    }

    pub fn is_lossy(self) -> bool {
        self.lossy().is_some()
    }

    pub fn lossy(self) -> Option<(LossyCodec, u32)> {
        match self {
            ExportCodec::Mp3(kbps) => Some((LossyCodec::Mp3, kbps)),
            ExportCodec::Aac(kbps) => Some((LossyCodec::Aac, kbps)),
            _ => None,
        }
    }

    /// Bits per sample written to the file (lossy encoders take 16-bit input)
    pub fn bits(self) -> u16 {
        match self {
            ExportCodec::Wav(d) => d.bits_per_sample(),
            ExportCodec::Flac(ExportBitDepth::Bit16) => 16,
            ExportCodec::Flac(_) => 24,
            ExportCodec::Mp3(_) | ExportCodec::Aac(_) => 16,
        }
    }

    /// Dither used when quantizing for this format if the caller does not override it. Lossy
    /// encoders get plain TPDF: noise-shaped dither would spend their bits on ultrasonic noise.
    pub fn default_dither(self) -> DitherType {
        match self {
            ExportCodec::Wav(d) => d.default_dither(),
            ExportCodec::Flac(ExportBitDepth::Bit16) => DitherType::NoiseShaped,
            ExportCodec::Flac(_) => DitherType::Tpdf,
            ExportCodec::Mp3(_) | ExportCodec::Aac(_) => DitherType::Tpdf,
        }
    }

    /// The sample rate the file is written at, given the one asked for: lossy encoders only
    /// accept a few rates, so others are resampled to the nearest supported rate above
    pub fn output_rate(self, requested: u32) -> u32 {
        match self.lossy() {
            Some((codec, _)) => {
                let rates = codec.sample_rates();
                if rates.contains(&requested) {
                    requested
                } else {
                    rates.iter().copied().find(|&r| r >= requested).unwrap_or(rates[rates.len() - 1])
                }
            }
            None => requested,
        }
    }

    /// `path` with this format's extension
    pub fn with_extension(self, path: &Path) -> PathBuf {
        path.with_extension(self.extension())
    }
}

/// Receives the finished master, block by block
pub(crate) trait EncoderSink {
    fn write(&mut self, left: &[f32], right: &[f32]) -> Result<(), String>;
    /// Complete the file; called once after the last `write`
    fn finish(&mut self) -> Result<(), String>;
}

pub(crate) fn open_sink(
    path: &Path,
    codec: ExportCodec,
    sample_rate: u32,
    dither: DitherType,
    bwf: Option<BwfMetadata>,
) -> Result<Box<dyn EncoderSink>, String> {
    let bits = codec.bits();
    let dither = MasteringDither::new(dither, if bits == 16 { 16 } else { 24 });
    Ok(match codec {
        ExportCodec::Wav(bit_depth) => {
            let spec = hound::WavSpec {
                channels: 2,
                sample_rate,
                bits_per_sample: bits,
                sample_format: match bit_depth {
                    ExportBitDepth::Bit16 | ExportBitDepth::Bit24 => hound::SampleFormat::Int,
                    ExportBitDepth::Float32 => hound::SampleFormat::Float,
                },
            };
            let writer = hound::WavWriter::create(path, spec).map_err(|e| format!("Failed to create output WAV: {}", e))?;
            Box::new(WavSink { writer: Some(writer), bit_depth, dither, path: path.to_path_buf(), bwf })
        }
        ExportCodec::Flac(_) => Box::new(FlacSink { samples: Vec::new(), bits, sample_rate, dither, path: path.to_path_buf() }),
        ExportCodec::Mp3(_) | ExportCodec::Aac(_) => {
            let (lossy, kbps) = codec.lossy().expect("lossy codec");
            let encoder = MfEncoder::create(path, lossy, sample_rate, kbps)?;
            // Windows' MP3 encoder drops its last partial frames: flush them with silence
            let tail_frames = if lossy == LossyCodec::Mp3 { 4096 } else { 0 };
            Box::new(LossySink { encoder: Some(encoder), dither, scratch: Vec::new(), tail_frames })
        }
    })
}

#[inline]
fn quantize(v: f32, bits: u16) -> i32 {
    let full = ((1i32 << (bits - 1)) - 1) as f32;
    (v.clamp(-1.0, 1.0) * full).round() as i32
}

struct WavSink {
    writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
    bit_depth: ExportBitDepth,
    dither: MasteringDither,
    path: PathBuf,
    bwf: Option<BwfMetadata>,
}

impl EncoderSink for WavSink {
    fn write(&mut self, left: &[f32], right: &[f32]) -> Result<(), String> {
        let writer = self.writer.as_mut().ok_or("WAV already finished")?;
        for (&l, &r) in left.iter().zip(right) {
            let (l, r) = self.dither.process_sample(l, r);
            match self.bit_depth {
                ExportBitDepth::Bit16 => {
                    writer.write_sample(quantize(l, 16) as i16).map_err(|e| e.to_string())?;
                    writer.write_sample(quantize(r, 16) as i16).map_err(|e| e.to_string())?;
                }
                ExportBitDepth::Bit24 => {
                    writer.write_sample(quantize(l, 24)).map_err(|e| e.to_string())?;
                    writer.write_sample(quantize(r, 24)).map_err(|e| e.to_string())?;
                }
                ExportBitDepth::Float32 => {
                    writer.write_sample(l).map_err(|e| e.to_string())?;
                    writer.write_sample(r).map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        let writer = self.writer.take().ok_or("WAV already finished")?;
        writer.finalize().map_err(|e| format!("Failed to finalize WAV: {}", e))?;
        // Broadcast Wave Format (BWF bext chunk) metadata
        let _ = self.bwf.take().unwrap_or_default().write_to_wav_file(&self.path);
        Ok(())
    }
}

/// FLAC frames are encoded in parallel once the whole master is in
struct FlacSink {
    samples: Vec<i32>,
    bits: u16,
    sample_rate: u32,
    dither: MasteringDither,
    path: PathBuf,
}

impl EncoderSink for FlacSink {
    fn write(&mut self, left: &[f32], right: &[f32]) -> Result<(), String> {
        self.samples.reserve(left.len() * 2);
        for (&l, &r) in left.iter().zip(right) {
            let (l, r) = self.dither.process_sample(l, r);
            self.samples.push(quantize(l, self.bits));
            self.samples.push(quantize(r, self.bits));
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        use flacenc::component::BitRepr;
        use flacenc::error::Verify;
        let config = flacenc::config::Encoder::default()
            .into_verified()
            .map_err(|(_, e)| format!("FLAC encoder configuration: {:?}", e))?;
        let samples = std::mem::take(&mut self.samples);
        let source = flacenc::source::MemSource::from_samples(&samples, 2, self.bits as usize, self.sample_rate as usize);
        let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
            .map_err(|e| format!("FLAC encoding failed: {:?}", e))?;
        let mut sink = flacenc::bitsink::ByteSink::new();
        stream.write(&mut sink).map_err(|e| format!("FLAC encoding failed: {:?}", e))?;
        let mut bytes = sink.as_slice().to_vec();
        // STREAMINFO's minimum block size must exclude the shorter last block (flacenc counts
        // it); with min != max, decoders treat the fixed-blocksize stream as variable and fail
        if bytes.len() >= 12 && &bytes[..4] == b"fLaC" {
            bytes.copy_within(10..12, 8);
        }
        std::fs::write(&self.path, bytes).map_err(|e| format!("Failed to write FLAC file: {}", e))
    }
}

struct LossySink {
    encoder: Option<MfEncoder>,
    dither: MasteringDither,
    scratch: Vec<i16>,
    /// Digital silence appended before finishing
    tail_frames: usize,
}

impl EncoderSink for LossySink {
    fn write(&mut self, left: &[f32], right: &[f32]) -> Result<(), String> {
        self.scratch.clear();
        for (&l, &r) in left.iter().zip(right) {
            let (l, r) = self.dither.process_sample(l, r);
            self.scratch.push(quantize(l, 16) as i16);
            self.scratch.push(quantize(r, 16) as i16);
        }
        self.encoder.as_mut().ok_or("encoder already finished")?.write(&self.scratch)
    }

    fn finish(&mut self) -> Result<(), String> {
        let mut encoder = self.encoder.take().ok_or("encoder already finished")?;
        if self.tail_frames > 0 {
            encoder.write(&vec![0i16; self.tail_frames * 2])?;
        }
        encoder.finish()
    }
}

/// Collects the master in memory, unquantized
#[derive(Default)]
pub(crate) struct MemorySink {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl EncoderSink for MemorySink {
    fn write(&mut self, left: &[f32], right: &[f32]) -> Result<(), String> {
        self.left.extend_from_slice(left);
        self.right.extend_from_slice(right);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        Ok(())
    }
}
