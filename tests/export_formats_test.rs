//! FLAC, MP3 and AAC export: files decode back to the master. MP3 / AAC need Windows Media
//! Foundation and skip themselves where it is missing.

use mastering_studio::audio::media_foundation;
use mastering_studio::audio::{encode_buffer, load_audio_file, render_to_file, AudioBuffer, ExportBitDepth, ExportCodec, RenderSource};

const SR: u32 = 44100;

/// Programme-like test signal: two tones and a little noise, different in each channel
fn signal(seconds: f32) -> AudioBuffer {
    let n = (SR as f32 * seconds) as usize;
    let mut seed: u64 = 9;
    let tau = 2.0 * std::f32::consts::PI;
    let (mut l, mut r) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for i in 0..n {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let noise = ((seed >> 33) as f32 / (1u64 << 31) as f32 * 2.0 - 1.0) * 0.02;
        let t = i as f32 / SR as f32;
        let a = (tau * 220.0 * t).sin() * 0.3;
        let b = (tau * 1760.0 * t).sin() * 0.1;
        l.push(a + b + noise);
        r.push(a - b + noise);
    }
    AudioBuffer::from_stereo(l, r, SR)
}

/// Best alignment of `decoded` against `reference` (decoded[i + lag] ~ reference[i]) and the SNR there
fn aligned_snr(reference: &AudioBuffer, decoded: &AudioBuffer, max_lag: usize) -> (usize, f64) {
    let (rl, _) = reference.to_stereo();
    let (dl, _) = decoded.to_stereo();
    let span = SR as usize / 2..SR as usize; // steady-state middle section
    let mut best = (0, f64::MIN);
    for lag in 0..=max_lag {
        let (mut sig, mut err) = (0.0f64, 0.0f64);
        for i in span.clone() {
            let Some(&d) = dl.get(i + lag) else { break };
            sig += (rl[i] as f64).powi(2);
            err += ((rl[i] - d) as f64).powi(2);
        }
        let snr = 10.0 * (sig / err.max(1e-20)).log10();
        if snr > best.1 {
            best = (lag, snr);
        }
    }
    best
}

#[test]
fn test_flac_export_is_lossless_at_24_bit() {
    let master = signal(1.5);
    let path = std::env::temp_dir().join("test_export_24.flac");
    render_to_file(&master, &path, ExportCodec::Flac(ExportBitDepth::Bit24), RenderSource::Bypass, None, None, |_| {}).unwrap();
    let decoded = load_audio_file(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(decoded.sample_rate, SR);
    assert_eq!(decoded.num_frames(), master.num_frames());
    let (lag, snr) = aligned_snr(&master, &decoded, 0);
    println!("FLAC 24-bit: lag {} SNR {:.1} dB", lag, snr);
    // 24-bit quantization plus TPDF dither: far below anything audible
    assert!(snr > 110.0, "24-bit FLAC SNR {:.1} dB", snr);
}

#[test]
fn test_flac_16_bit_matches_16_bit_wav() {
    let master = signal(1.0);
    let flac = std::env::temp_dir().join("test_export_16.flac");
    let wav = std::env::temp_dir().join("test_export_16.wav");
    encode_buffer(&master, &flac, ExportCodec::Flac(ExportBitDepth::Bit16), None).unwrap();
    encode_buffer(&master, &wav, ExportCodec::Wav(ExportBitDepth::Bit16), None).unwrap();
    let a = load_audio_file(&flac).unwrap();
    let b = load_audio_file(&wav).unwrap();
    let _ = std::fs::remove_file(&flac);
    let _ = std::fs::remove_file(&wav);
    // Same dither sequence, same quantizer: the FLAC holds exactly the WAV's samples
    assert_eq!(a.channels[0], b.channels[0]);
    assert_eq!(a.channels[1], b.channels[1]);
}

#[test]
fn test_lossy_exports_decode_back_to_the_master() {
    if !media_foundation::is_available() {
        eprintln!("Media Foundation not available, skipping MP3/AAC export test");
        return;
    }
    let master = signal(2.0);
    for codec in [ExportCodec::Mp3(320), ExportCodec::Aac(192)] {
        let path = std::env::temp_dir().join(format!("test_export.{}", codec.extension()));
        render_to_file(&master, &path, codec, RenderSource::Bypass, None, None, |_| {}).unwrap();
        let decoded = load_audio_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(decoded.sample_rate, SR);
        let (lag, snr) = aligned_snr(&master, &decoded, 4096);
        println!("{}: {} frames decoded for {}, encoder delay {} samples, SNR {:.1} dB", codec.name(), decoded.num_frames(), master.num_frames(), lag, snr);
        // Nothing at the end may be lost to the encoder's framing
        assert!(decoded.num_frames() >= master.num_frames() + lag, "{} file lost the end of the master", codec.name());
        assert!(snr > 15.0, "{} decodes to something else (SNR {:.1} dB)", codec.name(), snr);
    }
}

#[test]
fn test_lossy_export_resamples_to_a_supported_rate() {
    assert_eq!(ExportCodec::Aac(192).output_rate(96000), 48000);
    assert_eq!(ExportCodec::Aac(192).output_rate(22050), 44100);
    assert_eq!(ExportCodec::Mp3(320).output_rate(48000), 48000);
    assert_eq!(ExportCodec::Flac(ExportBitDepth::Bit24).output_rate(96000), 96000);
    if !media_foundation::is_available() {
        return;
    }
    let (l, r) = signal(1.0).to_stereo();
    let hi_res = mastering_studio::audio::SincResampler::default().resample_buffer(&AudioBuffer::from_stereo(l, r, SR), 96000);
    let path = std::env::temp_dir().join("test_export_96k.m4a");
    render_to_file(&hi_res, &path, ExportCodec::Aac(160), RenderSource::Bypass, None, None, |_| {}).unwrap();
    let decoded = load_audio_file(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(decoded.sample_rate, 48000);
}

#[test]
fn test_codec_preview_is_aligned_with_the_master() {
    use mastering_studio::audio::CodecPreview;
    if !media_foundation::is_available() {
        eprintln!("Media Foundation not available, skipping codec preview test");
        return;
    }
    let master = signal(3.0);
    assert!(CodecPreview::build(&master, ExportCodec::Flac(ExportBitDepth::Bit24), |_| {}).is_err(), "lossless has nothing to preview");
    for codec in [ExportCodec::Mp3(192), ExportCodec::Aac(128)] {
        let preview = CodecPreview::build(&master, codec, |_| {}).unwrap();
        println!("{} (delay {})", preview.summary(), preview.encoder_delay);
        // Same length and timeline as the master, so the transport can swap between them
        assert_eq!(preview.decoded.num_frames(), master.num_frames());
        assert_eq!(preview.decoded.sample_rate, master.sample_rate);
        let (lag, snr) = aligned_snr(&master, &preview.decoded, 64);
        assert_eq!(lag, 0, "{} preview is not aligned", codec.name());
        assert!(snr > 15.0, "{} preview SNR {:.1} dB", codec.name(), snr);
        // The difference is what the codec changed: well below the master, and master = codec + difference
        assert!(preview.difference_db < -15.0, "difference only {:.1} dB below the master", -preview.difference_db);
        let i = master.num_frames() / 2;
        let rebuilt = preview.decoded.channels[0][i] + preview.difference.channels[0][i];
        assert!((rebuilt - master.channels[0][i]).abs() < 1e-6);
    }
}
