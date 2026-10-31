use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use super::buffer::AudioBuffer;
use super::bwf::BwfMetadata;
use super::encoder::{open_sink, EncoderSink, ExportCodec, MemorySink};
use super::ml_separator::{StemBus, StemSet};
use super::resample::SincResampler;
use super::trim::TrimFadeSettings;
use crate::dsp::{DitherType, MasteringSuite};
use crate::vst3::Vst3Plugin;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExportBitDepth {
    Bit16,
    Bit24,
    Float32,
}

impl ExportBitDepth {
    pub fn bits_per_sample(self) -> u16 {
        match self {
            ExportBitDepth::Bit16 => 16,
            ExportBitDepth::Bit24 => 24,
            ExportBitDepth::Float32 => 32,
        }
    }

    /// Dither applied when quantizing to this depth if the caller does not override it
    pub fn default_dither(self) -> DitherType {
        match self {
            ExportBitDepth::Bit16 => DitherType::NoiseShaped,
            ExportBitDepth::Bit24 => DitherType::Tpdf,
            ExportBitDepth::Float32 => DitherType::None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RenderOptions {
    pub trim_fade: Option<TrimFadeSettings>,
    pub bwf_metadata: Option<BwfMetadata>,
    pub target_sample_rate: Option<u32>,
    /// Overrides the bit depth's default dither (`ExportBitDepth::default_dither`)
    pub dither: Option<DitherType>,
    /// Separated stems of `input_buffer`. With the built-in suite, Stem Rebalance / Stem EQ are
    /// then applied to the real stems instead of the real-time approximation.
    pub stems: Option<Arc<StemSet>>,
}

pub enum RenderSource<'a> {
    Vst3(&'a Vst3Plugin),
    Dsp(&'a mut MasteringSuite),
    Bypass,
}

impl RenderSource<'_> {
    /// Processing delay introduced by the source, removed from the rendered output
    fn latency_samples(&self) -> usize {
        match self {
            RenderSource::Vst3(plugin) => plugin.latency() as usize,
            RenderSource::Dsp(suite) => suite.latency_samples(),
            RenderSource::Bypass => 0,
        }
    }

    fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        match self {
            RenderSource::Vst3(plugin) => plugin.process_stereo(in_l, in_r, out_l, out_r),
            RenderSource::Dsp(suite) => {
                out_l.copy_from_slice(in_l);
                out_r.copy_from_slice(in_r);
                suite.process_stereo(out_l, out_r);
            }
            RenderSource::Bypass => {
                out_l.copy_from_slice(in_l);
                out_r.copy_from_slice(in_r);
            }
        }
    }
}

/// Render the master to a WAV file
pub fn render_to_wav<P: AsRef<Path>>(
    input_buffer: &AudioBuffer,
    output_path: P,
    bit_depth: ExportBitDepth,
    source: RenderSource,
    options: Option<RenderOptions>,
    cancel_flag: Option<Arc<AtomicBool>>,
    progress_cb: impl Fn(f32),
) -> Result<(), String> {
    render_to_file(input_buffer, output_path, ExportCodec::Wav(bit_depth), source, options, cancel_flag, progress_cb)
}

/// Render the master and encode it in `codec` (WAV, FLAC, MP3 or AAC). Lossy codecs only take
/// some sample rates, so the master may be resampled (`ExportCodec::output_rate`).
pub fn render_to_file<P: AsRef<Path>>(
    input_buffer: &AudioBuffer,
    output_path: P,
    codec: ExportCodec,
    source: RenderSource,
    options: Option<RenderOptions>,
    cancel_flag: Option<Arc<AtomicBool>>,
    progress_cb: impl Fn(f32),
) -> Result<(), String> {
    if input_buffer.num_frames() == 0 {
        return Err("Input audio buffer is empty".to_string());
    }
    let options = options.unwrap_or_default();
    let target_sr = codec.output_rate(options.target_sample_rate.unwrap_or(input_buffer.sample_rate));
    let dither = options.dither.unwrap_or_else(|| codec.default_dither());
    let bwf = options.bwf_metadata.clone();
    let mut sink = open_sink(output_path.as_ref(), codec, target_sr, dither, bwf)?;
    render_into(input_buffer, target_sr, source, &options, cancel_flag, progress_cb, sink.as_mut())?;
    sink.finish()
}

/// Render the master into memory, unquantized, at the input's rate unless the options ask for
/// another
pub fn render_to_buffer(
    input_buffer: &AudioBuffer,
    source: RenderSource,
    options: Option<RenderOptions>,
    cancel_flag: Option<Arc<AtomicBool>>,
    progress_cb: impl Fn(f32),
) -> Result<AudioBuffer, String> {
    if input_buffer.num_frames() == 0 {
        return Err("Input audio buffer is empty".to_string());
    }
    let options = options.unwrap_or_default();
    let target_sr = options.target_sample_rate.unwrap_or(input_buffer.sample_rate);
    let mut sink = MemorySink::default();
    render_into(input_buffer, target_sr, source, &options, cancel_flag, progress_cb, &mut sink)?;
    Ok(AudioBuffer::from_stereo(sink.left, sink.right, target_sr))
}

/// Encode an already rendered master (dithered for integer and lossy formats)
pub fn encode_buffer<P: AsRef<Path>>(buffer: &AudioBuffer, output_path: P, codec: ExportCodec, dither: Option<DitherType>) -> Result<(), String> {
    let sr = codec.output_rate(buffer.sample_rate);
    let (mut left, mut right) = buffer.to_stereo();
    if sr != buffer.sample_rate {
        (left, right) = SincResampler::default().resample_stereo(&left, &right, buffer.sample_rate, sr);
    }
    let mut sink = open_sink(output_path.as_ref(), codec, sr, dither.unwrap_or_else(|| codec.default_dither()), None)?;
    for (l, r) in left.chunks(4096).zip(right.chunks(4096)) {
        sink.write(l, r)?;
    }
    sink.finish()
}

/// Process `input_buffer` through `source` (trim, fades, latency compensation, resampling to
/// `target_sr`) and stream the result into `sink`
fn render_into(
    input_buffer: &AudioBuffer,
    target_sr: u32,
    mut source: RenderSource,
    options: &RenderOptions,
    cancel_flag: Option<Arc<AtomicBool>>,
    progress_cb: impl Fn(f32),
    sink: &mut dyn EncoderSink,
) -> Result<(), String> {
    let num_frames = input_buffer.num_frames();
    let sample_rate = input_buffer.sample_rate;
    let resampling = target_sr != sample_rate;

    let block_size = 1024;

    // Save previous configuration to restore after offline bounce
    let vst3_restore_params = match &source {
        RenderSource::Vst3(plugin) => Some((plugin.sample_rate(), plugin.max_block_size())),
        _ => None,
    };

    // Ensure processing engine is configured for the target audio file's native sample rate
    match &mut source {
        RenderSource::Vst3(plugin) => {
            plugin.setup_audio(sample_rate as f64, block_size)?;
            plugin.seek(0);
        }
        RenderSource::Dsp(suite) => {
            suite.set_sample_rate(sample_rate as f32);
            suite.reset_state();
        }
        RenderSource::Bypass => {}
    }

    // Trained stems drive the input when they belong to this buffer and a stem module is on
    let mut stem_feed: Option<(Arc<StemSet>, StemBus)> = None;
    match &mut source {
        RenderSource::Dsp(suite) => {
            let stems = options
                .stems
                .clone()
                .filter(|s| s.num_frames == num_frames && s.sample_rate == sample_rate);
            suite.external_stems = stems.is_some() && suite.uses_stems();
            if suite.external_stems {
                let mut bus = StemBus::new(sample_rate as f32);
                bus.configure(suite);
                stem_feed = stems.map(|s| (s, bus));
            }
        }
        RenderSource::Vst3(_) => {}
        RenderSource::Bypass => {}
    }

    // Determine render frame boundaries (Top & Tail trim)
    let trim_fade_opt = options.trim_fade;
    let (start_frame, end_frame) = if let Some(ref tf) = trim_fade_opt {
        tf.get_frame_boundaries(num_frames)
    } else {
        (0, num_frames)
    };
    let total_render_frames = end_frame.saturating_sub(start_frame).max(1);

    // Feed `latency` extra frames of silence past the end and drop the same number from the start,
    // so lookahead processors produce output aligned with the source
    let latency = source.latency_samples();
    let progress_scale = if resampling { 0.5 } else { 1.0 };

    let mut in_block_l = vec![0.0f32; block_size];
    let mut in_block_r = vec![0.0f32; block_size];
    let mut out_block_l = vec![0.0f32; block_size];
    let mut out_block_r = vec![0.0f32; block_size];

    let mut processed_l = Vec::with_capacity(if resampling { total_render_frames } else { 0 });
    let mut processed_r = Vec::with_capacity(if resampling { total_render_frames } else { 0 });

    let feed_end = end_frame + latency;
    let mut frame_cursor = start_frame;
    let mut to_skip = latency;
    let mut out_frame = start_frame; // source frame index of the next output frame

    while frame_cursor < feed_end {
        if let Some(ref flag) = cancel_flag {
            if flag.load(Ordering::Relaxed) {
                return Err("Render cancelled by user".to_string());
            }
        }

        let chunk = (feed_end - frame_cursor).min(block_size);
        for i in 0..chunk {
            let (l, r) = if frame_cursor + i >= end_frame {
                (0.0, 0.0)
            } else if let Some((stems, bus)) = stem_feed.as_mut() {
                bus.mix(stems.frames_at_index(frame_cursor + i))
            } else {
                input_buffer.get_stereo_frame(frame_cursor + i)
            };
            in_block_l[i] = l;
            in_block_r[i] = r;
        }

        source.process(
            &in_block_l[..chunk],
            &in_block_r[..chunk],
            &mut out_block_l[..chunk],
            &mut out_block_r[..chunk],
        );
        frame_cursor += chunk;

        let skip = to_skip.min(chunk);
        to_skip -= skip;
        let valid = (chunk - skip).min(end_frame - out_frame);
        let out_l = &mut out_block_l[skip..skip + valid];
        let out_r = &mut out_block_r[skip..skip + valid];

        // Apply fade-in and fade-out envelope curves if configured
        if let Some(ref tf) = trim_fade_opt {
            for i in 0..valid {
                let fade_gain = tf.calculate_gain(out_frame + i, start_frame, end_frame, sample_rate);
                out_l[i] *= fade_gain;
                out_r[i] *= fade_gain;
            }
        }

        if resampling {
            processed_l.extend_from_slice(out_l);
            processed_r.extend_from_slice(out_r);
        } else {
            sink.write(out_l, out_r)?;
        }

        out_frame += valid;
        let rendered_so_far = out_frame - start_frame;
        progress_cb((rendered_so_far as f32 / total_render_frames as f32 * progress_scale).clamp(0.0, progress_scale));
    }

    if resampling {
        // Audiophile Kaiser-windowed Sinc Resampler, dithered after conversion
        progress_cb(0.55);
        let resampler = SincResampler::default();
        let (resampled_l, resampled_r) = resampler.resample_stereo(&processed_l, &processed_r, sample_rate, target_sr);
        progress_cb(0.85);
        sink.write(&resampled_l, &resampled_r)?;
        progress_cb(1.0);
    }

    // Restore VST3 plugin sample rate and block size for real-time playback
    if let (Some((orig_sr, orig_bs)), RenderSource::Vst3(plugin)) = (vst3_restore_params, &mut source) {
        plugin.setup_audio(orig_sr, orig_bs)?;
    }

    Ok(())
}
