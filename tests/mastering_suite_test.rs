use std::path::Path;
use mastering_studio::audio::{
    load_audio_file, render_to_wav, AlbumMontage, AudioBuffer, ExportBitDepth,
    FadeShape, RenderOptions, RenderSource, SincResampler, StemMixer, StemSeparator,
    TrimFadeSettings, NUM_STEMS,
};
use mastering_studio::dsp::{MasterAssistant, MasteringSuite, XmlPreset};
use mastering_studio::vst3::Vst3Plugin;

#[test]
fn test_dsp_mastering_suite() {
    let sample_rate = 48000.0;
    let mut suite = MasteringSuite::new(sample_rate);

    let num_samples = 1024;
    // Generate a test stereo signal (1 kHz sine wave + 100 Hz bass)
    let mut left: Vec<f32> = (0..num_samples)
        .map(|i| {
            let t = i as f32 / sample_rate;
            0.5 * (2.0 * std::f32::consts::PI * 1000.0 * t).sin()
                + 0.5 * (2.0 * std::f32::consts::PI * 80.0 * t).sin()
        })
        .collect();
    let mut right = left.clone();

    suite.process_stereo(&mut left, &mut right);

    for (idx, (l, r)) in left.iter().zip(right.iter()).enumerate() {
        assert!(!l.is_nan() && !l.is_infinite(), "Left sample {} is NaN/Inf", idx);
        assert!(!r.is_nan() && !r.is_infinite(), "Right sample {} is NaN/Inf", idx);
        assert!(l.abs() <= 1.05);
        assert!(r.abs() <= 1.05);
    }
}

#[test]
fn test_render_to_wav_and_decode() {
    let sample_rate = 44100;
    let num_samples = 44100; // 1 second
    let left: Vec<f32> = (0..num_samples)
        .map(|i| (2.0 * std::f32::consts::PI * 440.0 * (i as f32 / sample_rate as f32)).sin() * 0.4)
        .collect();
    let right = left.clone();

    let in_buf = AudioBuffer::from_stereo(left, right, sample_rate);
    let temp_wav = std::env::temp_dir().join("test_suite_render.wav");

    let mut suite = MasteringSuite::new(sample_rate as f32);
    let r = render_to_wav(
        &in_buf,
        &temp_wav,
        ExportBitDepth::Bit24,
        RenderSource::Dsp(&mut suite),
        None,
        None,
        |_| {},
    );
    assert!(r.is_ok(), "render_to_wav failed: {:?}", r);

    // Verify reading back using universal decoder
    let read_buf = load_audio_file(&temp_wav).expect("load_audio_file failed");
    assert_eq!(read_buf.sample_rate, sample_rate);
    assert_eq!(read_buf.num_frames(), num_samples);

    let _ = std::fs::remove_file(&temp_wav);
}

/// Optional fallback-plugin tests run only when MASTERING_STUDIO_TEST_VST3 names a .vst3 file
fn test_vst3_path() -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(std::env::var_os("MASTERING_STUDIO_TEST_VST3")?);
    if p.exists() { Some(p) } else { None }
}

#[test]
fn test_fallback_vst3_processing_state_and_view() {
    use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW, CW_USEDEFAULT};
    use windows::core::w;

    let Some(vst3_path) = test_vst3_path() else {
        eprintln!("MASTERING_STUDIO_TEST_VST3 not set, skipping fallback plugin test");
        return;
    };

    let plugin = Vst3Plugin::load(&vst3_path).expect("Failed to load VST3 plugin");
    let in_l = vec![0.1f32; 512];
    let in_r = vec![0.1f32; 512];
    let mut out_l = vec![0.0f32; 512];
    let mut out_r = vec![0.0f32; 512];
    plugin.process_stereo(&in_l, &in_r, &mut out_l, &mut out_r);
    assert!(out_l.iter().chain(out_r.iter()).all(|s| s.is_finite()));

    let state_bytes = plugin.save_state().expect("save_state failed");
    plugin.load_state(&state_bytes).expect("load_state failed");

    unsafe {
        let hwnd = CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Test Harness"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1200,
            800,
            None,
            None,
            None,
            None,
        ).expect("CreateWindowExW failed");
        if let Ok(view_rect) = plugin.create_view(hwnd) {
            assert!(view_rect.width() > 0 && view_rect.height() > 0);
            plugin.destroy_view();
        }
        let _ = DestroyWindow(hwnd);
    }
}

#[test]
fn test_waveform_peaks_generation() {
    let sample_rate = 44100;
    let num_samples = 44100; // 1 second
    let left: Vec<f32> = (0..num_samples)
        .map(|i| (2.0 * std::f32::consts::PI * 440.0 * (i as f32 / sample_rate as f32)).sin() * 0.8)
        .collect();
    let right: Vec<f32> = (0..num_samples)
        .map(|i| (2.0 * std::f32::consts::PI * 880.0 * (i as f32 / sample_rate as f32)).sin() * 0.5)
        .collect();

    let buf = AudioBuffer::from_stereo(left, right, sample_rate);
    let peaks = buf.peaks();
    assert_eq!(peaks.len(), 440);

    for p in peaks {
        assert!(p.max_l > 0.0 && p.max_l <= 0.81);
        assert!(p.min_l < 0.0 && p.min_l >= -0.81);
        assert!(p.max_r > 0.0 && p.max_r <= 0.51);
        assert!(p.min_r < 0.0 && p.min_r >= -0.51);
    }

    // Custom bin count
    let custom_peaks = buf.generate_waveform_peaks(100);
    assert_eq!(custom_peaks.len(), 100);
}

#[test]
fn test_end_to_end_demo_mastering_pipeline() {
    let demo_path = Path::new("demo_mastering_track.wav");
    if !demo_path.exists() {
        return;
    }

    println!("Loading demo mastering track: {}", demo_path.display());
    let buf = load_audio_file(demo_path).expect("Failed to load demo_mastering_track.wav");
    assert_eq!(buf.sample_rate, 48000);
    assert!(buf.duration_seconds() >= 7.9);
    assert_eq!(buf.num_channels(), 2);
    assert!(!buf.peaks().is_empty());

    let mut suite = MasteringSuite::new(buf.sample_rate as f32);
    let output_wav = std::env::temp_dir().join("demo_mastered_suite_test.wav");

    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    let progress_pct = Arc::new(AtomicU32::new(0));
    let p_clone = Arc::clone(&progress_pct);

    let r = render_to_wav(
        &buf,
        &output_wav,
        ExportBitDepth::Bit24,
        RenderSource::Dsp(&mut suite),
        None,
        None,
        move |prog| {
            p_clone.store((prog * 100.0) as u32, Ordering::Relaxed);
        },
    );

    assert!(r.is_ok(), "render_to_wav with the built-in suite failed: {:?}", r);
    assert!(progress_pct.load(Ordering::Relaxed) >= 99);
    assert!(output_wav.exists());

    let mastered_buf = load_audio_file(&output_wav).expect("Failed to decode mastered WAV");
    assert_eq!(mastered_buf.sample_rate, 48000);
    assert_eq!(mastered_buf.num_frames(), buf.num_frames());

    // Verify audio amplitude
    let (s_l, s_r) = mastered_buf.get_stereo_frame(1000);
    assert!(!s_l.is_nan() && !s_r.is_nan());

    let _ = std::fs::remove_file(&output_wav);
    println!("End-to-end mastering pipeline verified successfully!");
}

#[test]
fn test_buffer_integrated_lufs_and_auto_gain() {
    use mastering_studio::audio::AudioEngine;
    use mastering_studio::dsp::lufs::calculate_buffer_integrated_lufs;

    let sample_rate = 48000;
    let num_samples = 48000 * 2; // 2 seconds

    // Main mix: moderate amplitude 1 kHz tone (~ -20 LUFS)
    let main_l: Vec<f32> = (0..num_samples)
        .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * (i as f32 / sample_rate as f32)).sin() * 0.1)
        .collect();
    let main_r = main_l.clone();
    let main_buf = AudioBuffer::from_stereo(main_l, main_r, sample_rate);

    // Reference track: loud commercial master (~ -9 LUFS)
    let ref_l: Vec<f32> = (0..num_samples)
        .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * (i as f32 / sample_rate as f32)).sin() * 0.6)
        .collect();
    let ref_r = ref_l.clone();
    let ref_buf = AudioBuffer::from_stereo(ref_l, ref_r, sample_rate);

    let lufs_main = calculate_buffer_integrated_lufs(&main_buf);
    let lufs_ref = calculate_buffer_integrated_lufs(&ref_buf);

    println!("Calculated Integrated LUFS: Main = {:.2} LUFS, Ref = {:.2} LUFS", lufs_main, lufs_ref);
    assert!(lufs_main < lufs_ref, "Main track should be quieter than reference");
    assert!(lufs_main > -40.0 && lufs_main < -15.0);
    assert!(lufs_ref > -15.0 && lufs_ref < 0.0);

    let engine = AudioEngine::new().expect("Failed to initialize AudioEngine");
    engine.set_audio_buffer(main_buf);
    engine.set_reference_buffer(ref_buf);

    let (m_lufs, r_lufs, delta_db) = engine.auto_match_reference_gain().expect("auto_match_reference_gain failed");
    println!("Auto-Match ΔLUFS: Main {:.2} vs Ref {:.2} (Δ {:.2} dB)", m_lufs, r_lufs, delta_db);
    assert!(delta_db < 0.0, "Reference track must be attenuated to match quieter main mix");

    let gain_offset = engine.reference_gain_offset();
    assert!(gain_offset < 1.0 && gain_offset > 0.05);

    // Test Streaming Loudness Target matching
    let (cur_lufs, needed_boost) = engine.apply_loudness_target(-14.0, -1.0).expect("apply_loudness_target failed");
    println!("Target Spotify: Current = {:.2} LUFS, Boost Applied = {:.2} dB", cur_lufs, needed_boost);
    assert!(needed_boost > 0.0);

    engine.with_dsp_suite(|s| {
        assert_eq!(s.maximizer.ceiling_db, -1.0);
        assert!((s.maximizer.threshold_db - (-needed_boost)).abs() < 1e-3);
    });
}

#[test]
fn test_realtime_spectrum_analyzer_bins() {
    let sample_rate = 48000.0;
    let mut suite = MasteringSuite::new(sample_rate);

    // Feed stereo audio blocks with diverse harmonics
    for block in 0..10 {
        let left: Vec<f32> = (0..512)
            .map(|i| {
                let t = (block * 512 + i) as f32 / sample_rate;
                (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.3
                    + (2.0 * std::f32::consts::PI * 3500.0 * t).sin() * 0.2
            })
            .collect();
        let right = left.clone();
        suite.spectral_analyzer.feed_samples(&left, &right);
    }

    suite.spectral_analyzer.update_spectrum();
    let spec = suite.spectral_analyzer.get_spectrum_db();

    assert_eq!(spec.len(), 128);
    for (i, &mag) in spec.iter().enumerate() {
        assert!(!mag.is_nan() && !mag.is_infinite(), "Bin {} is NaN or Inf: {}", i, mag);
        assert!(mag >= -95.0 && mag <= 12.0, "Bin {} out of range: {}", i, mag);
    }
}

#[test]
fn test_trim_and_fade_curves() {
    let mut tf = TrimFadeSettings {
        trim_start_fraction: 0.1,
        trim_end_fraction: 0.9,
        fade_in_seconds: 1.0,
        fade_out_seconds: 1.0,
        fade_shape: FadeShape::SCurve,
    };

    let total_frames = 10000;
    let (start_f, end_f) = tf.get_frame_boundaries(total_frames);
    assert_eq!(start_f, 1000);
    assert_eq!(end_f, 9000);

    let sr = 1000; // 1000 Hz, so 1.0s fade = 1000 frames
    // 1. Before start: 0.0 gain
    assert_eq!(tf.calculate_gain(500, start_f, end_f, sr), 0.0);
    // 2. After end: 0.0 gain
    assert_eq!(tf.calculate_gain(9500, start_f, end_f, sr), 0.0);
    // 3. Exactly at start: 0.0 gain
    assert_eq!(tf.calculate_gain(1000, start_f, end_f, sr), 0.0);
    // 4. Middle of fade-in: ~0.5 for S-curve
    let mid_fade = tf.calculate_gain(1500, start_f, end_f, sr);
    assert!((mid_fade - 0.5).abs() < 0.05);
    // 5. Sustained body: 1.0 gain
    assert_eq!(tf.calculate_gain(5000, start_f, end_f, sr), 1.0);
    // 6. Middle of fade-out: ~0.5 for S-curve
    let mid_out = tf.calculate_gain(8500, start_f, end_f, sr);
    assert!((mid_out - 0.5).abs() < 0.05);

    // Test Linear Fade
    tf.fade_shape = FadeShape::Linear;
    let lin_mid = tf.calculate_gain(1500, start_f, end_f, sr);
    assert!((lin_mid - 0.5).abs() < 1e-4);

    // Test Exponential Fade
    tf.fade_shape = FadeShape::Exponential;
    let exp_mid = tf.calculate_gain(1500, start_f, end_f, sr);
    assert!(exp_mid > 0.0 && exp_mid < 0.5);
}

#[test]
fn test_album_montage_cue_and_export() {
    let temp_dir = std::env::temp_dir();
    let track1_wav = temp_dir.join("montage_test_track1.wav");
    let track2_wav = temp_dir.join("montage_test_track2.wav");
    let album_wav = temp_dir.join("montage_album_master.wav");

    let sample_rate = 44100;
    let num_samples = 44100 * 2; // 2 seconds each
    let t1_left: Vec<f32> = (0..num_samples)
        .map(|i| (2.0 * std::f32::consts::PI * 440.0 * (i as f32 / sample_rate as f32)).sin() * 0.3)
        .collect();
    let t1_right = t1_left.clone();
    let buf1 = AudioBuffer::from_stereo(t1_left, t1_right, sample_rate);

    let t2_left: Vec<f32> = (0..num_samples)
        .map(|i| (2.0 * std::f32::consts::PI * 880.0 * (i as f32 / sample_rate as f32)).sin() * 0.5)
        .collect();
    let t2_right = t2_left.clone();
    let buf2 = AudioBuffer::from_stereo(t2_left, t2_right, sample_rate);

    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    {
        let mut w1 = hound::WavWriter::create(&track1_wav, spec).unwrap();
        for i in 0..num_samples {
            w1.write_sample((buf1.channels[0][i] * 32767.0) as i16).unwrap();
            w1.write_sample((buf1.channels[1][i] * 32767.0) as i16).unwrap();
        }
        w1.finalize().unwrap();

        let mut w2 = hound::WavWriter::create(&track2_wav, spec).unwrap();
        for i in 0..num_samples {
            w2.write_sample((buf2.channels[0][i] * 32767.0) as i16).unwrap();
            w2.write_sample((buf2.channels[1][i] * 32767.0) as i16).unwrap();
        }
        w2.finalize().unwrap();
    }

    let mut montage = AlbumMontage::new("Neon Horizons", "Mastering Studio");
    let _id1 = montage.add_track(track1_wav.clone(), 2.0, sample_rate);
    let _id2 = montage.add_track(track2_wav.clone(), 2.0, sample_rate);

    assert_eq!(montage.tracks.len(), 2);
    // Track 2 has default 2.0s pause gap, so total duration = 2.0 + 2.0 (gap) + 2.0 = 6.0s
    assert_eq!(montage.total_duration_seconds(), 6.0);

    // Set integrated LUFS for balancing test
    montage.tracks[0].integrated_lufs = -18.0;
    montage.tracks[1].integrated_lufs = -12.0;
    montage.target_album_lufs = -14.0;
    montage.auto_balance_album_lufs();

    // Track 1 (-18) needs +4 dB boost; Track 2 (-12) needs -2 dB cut
    assert!((montage.tracks[0].gain_offset_db - 4.0).abs() < 1e-3);
    assert!((montage.tracks[1].gain_offset_db - (-2.0)).abs() < 1e-3);

    // Verify CUE sheet generation
    let cue_str = montage.generate_cue_sheet("montage_album_master.wav");
    assert!(cue_str.contains("TITLE \"Neon Horizons\""));
    assert!(cue_str.contains("TRACK 01 AUDIO"));
    assert!(cue_str.contains("TRACK 02 AUDIO"));
    assert!(cue_str.contains("INDEX 00"));
    assert!(cue_str.contains("INDEX 01"));

    // Export entire album with CUE sheet and BWF metadata
    let cue_res = montage.export_album(&album_wav, ExportBitDepth::Bit24, |_, _| {});
    assert!(cue_res.is_ok(), "export_album failed: {:?}", cue_res);
    let cue_path = cue_res.unwrap();
    assert!(album_wav.exists());
    assert!(cue_path.exists());

    let cue_content = std::fs::read_to_string(&cue_path).unwrap();
    assert!(cue_content.contains("FILE \"montage_album_master.wav\" WAVE"));

    // Clean up temporary test files
    let _ = std::fs::remove_file(&track1_wav);
    let _ = std::fs::remove_file(&track2_wav);
    let _ = std::fs::remove_file(&album_wav);
    let _ = std::fs::remove_file(&cue_path);
}

#[test]
fn test_factory_xml_preset_import() {
    // Element/param layout used by factory XML presets
    let xml = r#"<?xml version="1.0"?>
<Preset Comments="Warm, wide master" PluginVersion="12.0">
  <Param ElementID="Maximizer" ParamID="Threshold" Value="-1.59325409" />
  <Param ElementID="Maximizer" ParamID="Margin" Value="-0.13453484" />
  <Param ElementID="Equalizer" ParamID="Band 2 Gain" Value="1.8" />
  <Param ElementID="Imager" ParamID="Width" Value="120" />
</Preset>"#;
    let preset = XmlPreset::parse_str(xml, "Classic Master".to_string()).expect("parse failed");
    assert_eq!(preset.comments, "Warm, wide master");
    assert_eq!(preset.plugin_ver, "12.0");
    assert!(preset.params.contains_key("Maximizer::Threshold"));

    let mut suite = MasteringSuite::new(48000.0);
    preset.apply_to_suite(&mut suite);
    assert!((suite.maximizer.ceiling_db - (-0.13453484)).abs() < 1e-3);
    assert!((suite.maximizer.threshold_db - (-1.59325409)).abs() < 1e-3);
    assert!((suite.dynamic_eq.bands[1].static_gain_db - 1.8).abs() < 1e-3);
    assert!((suite.imager.width - 1.2).abs() < 1e-3);
}

#[test]
fn test_master_assistant_profiler() {
    let sample_rate = 44100;
    let num_samples = 44100 * 2; // 2 seconds

    // Build a stereo signal with wide dynamic range (transients + bass + stereo width)
    let left: Vec<f32> = (0..num_samples)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            let sub = (2.0 * std::f32::consts::PI * 55.0 * t).sin() * 0.4;
            let mid = (2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.2;
            let transient = if i % 11025 == 0 { 0.8 } else { 0.0 };
            (sub + mid + transient) * 0.35
        })
        .collect();

    let right: Vec<f32> = (0..num_samples)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            let sub = (2.0 * std::f32::consts::PI * 55.0 * t).sin() * 0.4;
            let mid = (2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.2 * 0.8;
            let transient = if i % 11025 == 0 { 0.7 } else { 0.0 };
            (sub + mid + transient) * 0.35
        })
        .collect();

    let buf = AudioBuffer::from_stereo(left, right, sample_rate);

    let report = MasterAssistant::analyze(&buf, -14.0).expect("MasterAssistant::analyze failed");
    println!("Master Assistant Analysis Report:");
    println!("   Current Loudness: {:.2} LUFS (Target: {:.2} LUFS)", report.current_integrated_lufs, report.target_integrated_lufs);
    println!("   Peak: {:.2} dBTP, RMS: {:.2} dB", report.peak_level_dbtp, report.rms_level_db);
    println!("   Crest Factor: {:.2} dB", report.crest_factor_db);
    println!("   Sub-Bass Phase Correlation: {:.2}", report.sub_bass_phase_corr);
    println!("   Recommended Threshold: {:.2} dB, Ceiling: {:.2} dBTP", report.recommended_threshold_db, report.recommended_ceiling_db);
    println!("   Recommended Width: {:.0}%, Mono Cutoff: {:.0} Hz", report.recommended_stereo_width * 100.0, report.recommended_mono_maker_hz);

    assert!(report.current_integrated_lufs < -10.0);
    assert!(report.crest_factor_db > 5.0);
    assert!(report.recommended_threshold_db < 0.0);
    assert!(report.recommended_ceiling_db <= -1.0);
    assert!(report.sub_bass_phase_corr > 0.0);
    assert!(!report.recommended_eq_adjustments.is_empty());

    let mut suite = MasteringSuite::new(sample_rate as f32);
    MasterAssistant::apply_to_suite(&report, &mut suite);

    assert_eq!(suite.maximizer.ceiling_db, report.recommended_ceiling_db);
    assert_eq!(suite.maximizer.threshold_db, report.recommended_threshold_db);
    assert_eq!(suite.imager.width, report.recommended_stereo_width);
    assert_eq!(suite.imager.mono_bass_freq, report.recommended_mono_maker_hz);
    assert!(suite.dynamic_eq.bands[0].enabled);
    assert_eq!(suite.dynamic_eq.bands[0].freq, 80.0);
}

#[test]
fn test_sinc_resampler_quality() {
    let from_sr = 48000;
    let to_sr = 44100;
    let num_samples = 48000; // 1 second of 1 kHz tone

    let left: Vec<f32> = (0..num_samples)
        .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * (i as f32 / from_sr as f32)).sin() * 0.7)
        .collect();
    let right = left.clone();

    let resampler = SincResampler::default();
    let (out_l, out_r) = resampler.resample_stereo(&left, &right, from_sr, to_sr);

    let expected_len = (num_samples as f64 * (to_sr as f64 / from_sr as f64)).round() as usize;
    assert_eq!(out_l.len(), expected_len);
    assert_eq!(out_r.len(), expected_len);

    for (l, r) in out_l.iter().zip(out_r.iter()) {
        assert!(!l.is_nan() && !l.is_infinite());
        assert!(!r.is_nan() && !r.is_infinite());
        assert!(l.abs() <= 1.0);
        assert!(r.abs() <= 1.0);
    }

    // Verify RMS energy preservation across resampling
    let orig_rms = (left.iter().map(|s| s * s).sum::<f32>() / left.len() as f32).sqrt();
    let resampled_rms = (out_l.iter().map(|s| s * s).sum::<f32>() / out_l.len() as f32).sqrt();
    let ratio = resampled_rms / orig_rms;
    assert!((ratio - 1.0).abs() < 0.05, "RMS changed unexpectedly: {}", ratio);
}

#[test]
fn test_render_to_wav_with_resampling() {
    let source_sr = 96000;
    let target_sr = 44100;
    let num_samples = 96000; // 1 second audio at 96 kHz

    let left: Vec<f32> = (0..num_samples)
        .map(|i| (2.0 * std::f32::consts::PI * 440.0 * (i as f32 / source_sr as f32)).sin() * 0.5)
        .collect();
    let right = left.clone();

    let in_buf = AudioBuffer::from_stereo(left, right, source_sr);
    let temp_wav = std::env::temp_dir().join("test_suite_resampled_44k.wav");

    let mut suite = MasteringSuite::new(source_sr as f32);
    let opts = RenderOptions {
        trim_fade: None,
        bwf_metadata: None,
        target_sample_rate: Some(target_sr),
        dither: None,
        stems: None,
    };

    let r = render_to_wav(
        &in_buf,
        &temp_wav,
        ExportBitDepth::Bit24,
        RenderSource::Dsp(&mut suite),
        Some(opts),
        None,
        |_| {},
    );
    assert!(r.is_ok(), "render_to_wav with resampling failed: {:?}", r);

    let read_buf = load_audio_file(&temp_wav).expect("Failed to read back resampled WAV");
    assert_eq!(read_buf.sample_rate, target_sr);
    assert_eq!(read_buf.num_channels(), 2);
    assert!((read_buf.duration_seconds() - 1.0).abs() < 0.02);

    let _ = std::fs::remove_file(temp_wav);
}

#[test]
fn test_stem_mixer_summation() {
    let mut mixer = StemMixer::new();
    assert_eq!(mixer.stems.len(), NUM_STEMS);
    assert_eq!(NUM_STEMS, 6);
    assert!(!mixer.has_loaded_stems());

    let sample_rate = 44100;
    let num_frames = 4410; // 100 ms

    // Stem 0: Drums (pure 100 Hz kick tone)
    let drums_l: Vec<f32> = (0..num_frames).map(|i| (2.0 * std::f32::consts::PI * 100.0 * (i as f32 / sample_rate as f32)).sin() * 0.3).collect();
    let drums_buf = AudioBuffer::from_stereo(drums_l.clone(), drums_l, sample_rate);
    mixer.stems[0].buffer = Some(drums_buf);
    mixer.stems[0].gain_db = 0.0;

    // Stem 1: Bass (200 Hz tone)
    let bass_l: Vec<f32> = (0..num_frames).map(|i| (2.0 * std::f32::consts::PI * 200.0 * (i as f32 / sample_rate as f32)).sin() * 0.2).collect();
    let bass_buf = AudioBuffer::from_stereo(bass_l.clone(), bass_l, sample_rate);
    mixer.stems[1].buffer = Some(bass_buf);
    mixer.stems[1].gain_db = -3.0;

    // Stem 2: Vocals (1000 Hz tone panned left)
    let vocals_l: Vec<f32> = (0..num_frames).map(|i| (2.0 * std::f32::consts::PI * 1000.0 * (i as f32 / sample_rate as f32)).sin() * 0.25).collect();
    let vocals_buf = AudioBuffer::from_stereo(vocals_l.clone(), vocals_l, sample_rate);
    mixer.stems[2].buffer = Some(vocals_buf);
    mixer.stems[2].pan = -1.0; // Hard left

    assert!(mixer.has_loaded_stems());

    // 1. Test normal summation
    let master = mixer.sum_to_master_buffer().expect("sum_to_master_buffer failed");
    assert_eq!(master.num_frames(), num_frames);
    assert_eq!(master.sample_rate, sample_rate);

    let (m_l, m_r) = master.to_stereo();
    // Because vocals are hard left, left channel should have higher energy than right channel
    let left_energy = m_l.iter().map(|s| s * s).sum::<f32>();
    let right_energy = m_r.iter().map(|s| s * s).sum::<f32>();
    assert!(left_energy > right_energy);

    // 2. Test Solo
    mixer.toggle_stem_solo(1); // Solo bass
    let solo_master = mixer.sum_to_master_buffer().expect("solo summation failed");
    let (s_l, _) = solo_master.to_stereo();
    let solo_energy = s_l.iter().map(|s| s * s).sum::<f32>();
    assert!(solo_energy < left_energy);

    // 3. Test Mute
    mixer.toggle_stem_solo(1); // Unsolo bass
    mixer.toggle_stem_mute(0); // Mute drums
    let muted_master = mixer.sum_to_master_buffer().expect("mute summation failed");
    let (mut_l, _) = muted_master.to_stereo();
    let muted_energy = mut_l.iter().map(|s| s * s).sum::<f32>();
    assert!(muted_energy < left_energy);
}

#[test]
fn test_stem_separator_and_perfect_reconstruction() {
    let sample_rate = 44100;
    let num_samples = 8820; // 200 ms of composite audio

    // Create a composite stereo mix:
    // 1. Sub-Bass (60 Hz tone, dead center)
    // 2. Lead Vocal Formant (1000 Hz tone, dead center)
    // 3. Percussive drum clicks every 1000 samples
    // 4. Guitars & Keys chord (440 Hz + 660 Hz, panned left)
    // 5. Synth texture (3500 Hz, panned right)
    let mut left = vec![0.0f32; num_samples];
    let mut right = vec![0.0f32; num_samples];

    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        let sub = (2.0 * std::f32::consts::PI * 60.0 * t).sin() * 0.3;
        let vocal = (2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.25;
        let drum_click = if i % 1100 == 0 { 0.7 } else { 0.0 };
        let guitar = ((2.0 * std::f32::consts::PI * 440.0 * t).sin() + (2.0 * std::f32::consts::PI * 660.0 * t).sin()) * 0.15;
        let synth = (2.0 * std::f32::consts::PI * 3500.0 * t).sin() * 0.15;

        // Sub and Vocal are dead center
        // Guitar is panned left (L: 1.0, R: 0.3)
        // Synth is panned right (L: 0.2, R: 1.0)
        left[i] = sub + vocal + drum_click + guitar * 1.0 + synth * 0.2;
        right[i] = sub + vocal + drum_click + guitar * 0.3 + synth * 1.0;
    }

    let mix_buf = AudioBuffer::from_stereo(left.clone(), right.clone(), sample_rate);

    let res = StemSeparator::separate_stems(&mix_buf, |pct, step| {
        println!("Separation Progress: {:.0}% - {}", pct * 100.0, step);
    }).expect("Stem separation failed");

    // Verify all 6 stems exist with valid dimensions
    assert_eq!(res.drums.num_frames(), num_samples);
    assert_eq!(res.bass.num_frames(), num_samples);
    assert_eq!(res.vocals.num_frames(), num_samples);
    assert_eq!(res.instruments.num_frames(), num_samples);
    assert_eq!(res.synths.num_frames(), num_samples);
    assert_eq!(res.ambience.num_frames(), num_samples);

    // Verify Bass has significant energy below 250 Hz
    let (b_l, b_r) = res.bass.to_stereo();
    let bass_energy: f32 = b_l.iter().chain(b_r.iter()).map(|s| s * s).sum();
    assert!(bass_energy > 0.01, "Bass stem should contain substantial low-frequency energy");

    // Verify Vocals have significant mid energy
    let (v_l, v_r) = res.vocals.to_stereo();
    let vocal_energy: f32 = v_l.iter().chain(v_r.iter()).map(|s| s * s).sum();
    assert!(vocal_energy > 0.01, "Vocal stem should contain substantial mid energy");

    // Verify Drums have transients
    let (d_l, d_r) = res.drums.to_stereo();
    let drum_max = d_l.iter().chain(d_r.iter()).fold(0.0f32, |acc, &s| acc.max(s.abs()));
    assert!(drum_max > 0.1, "Drum stem should contain transient spikes");

    // Verify Mathematical Guarantee: Perfect Reconstruction (Interior samples)
    let (d_l, d_r) = res.drums.to_stereo();
    let (i_l, i_r) = res.instruments.to_stereo();
    let (s_l, s_r) = res.synths.to_stereo();
    let (a_l, a_r) = res.ambience.to_stereo();

    let interior_start = 2048; // Skip boundary window taper
    let interior_end = num_samples - 2048;

    let mut max_diff_l = 0.0f32;
    let mut max_diff_r = 0.0f32;

    for i in interior_start..interior_end {
        let recon_l = d_l[i] + b_l[i] + v_l[i] + i_l[i] + s_l[i] + a_l[i];
        let recon_r = d_r[i] + b_r[i] + v_r[i] + i_r[i] + s_r[i] + a_r[i];

        let diff_l = (recon_l - left[i]).abs();
        let diff_r = (recon_r - right[i]).abs();

        if diff_l > max_diff_l { max_diff_l = diff_l; }
        if diff_r > max_diff_r { max_diff_r = diff_r; }
    }

    println!("Maximum reconstruction error: L = {:.6}, R = {:.6}", max_diff_l, max_diff_r);
    assert!(max_diff_l < 0.08, "Left channel reconstruction error exceeds tolerance: {}", max_diff_l);
    assert!(max_diff_r < 0.08, "Right channel reconstruction error exceeds tolerance: {}", max_diff_r);
}

#[test]
fn test_stem_mixer_6_stems_export() {
    let mut mixer = StemMixer::new();
    let sample_rate = 44100;
    let num_frames = 4410;

    for (idx, stem) in mixer.stems.iter_mut().enumerate() {
        let freq = 100.0 * (idx + 1) as f32;
        let tone: Vec<f32> = (0..num_frames)
            .map(|i| (2.0 * std::f32::consts::PI * freq * (i as f32 / sample_rate as f32)).sin() * 0.2)
            .collect();
        stem.buffer = Some(AudioBuffer::from_stereo(tone.clone(), tone, sample_rate));
    }

    let temp_dir = std::env::temp_dir().join("mastering_studio_test_6stems_export");
    let export_res = mixer.export_all_stems(&temp_dir, "FullMix");
    assert!(export_res.is_ok(), "export_all_stems failed: {:?}", export_res);

    let files = export_res.unwrap();
    assert_eq!(files.len(), 6, "Expected exactly 6 exported stem WAV files");

    for file in &files {
        assert!(file.exists(), "Exported stem file missing: {}", file.display());
        let read_buf = load_audio_file(file).expect("Failed to read back exported stem WAV");
        assert_eq!(read_buf.sample_rate, sample_rate);
        assert_eq!(read_buf.num_frames(), num_frames);
        let _ = std::fs::remove_file(file);
    }
    let _ = std::fs::remove_dir(temp_dir);
}

#[test]
fn test_dsp_xml_preset_roundtrip() {
    use mastering_studio::dsp::EqChannelMode;

    let mut suite = MasteringSuite::new(48000.0);
    suite.maximizer.threshold_db = -7.5;
    suite.maximizer.ceiling_db = -0.8;
    suite.dynamic_eq.bands[3].static_gain_db = 2.5;
    suite.dynamic_eq.bands[3].freq = 900.0;
    suite.dynamic_eq.bands[5].channel_mode = EqChannelMode::SideOnly;
    suite.multiband_dynamics.bands[2].ratio = 3.0;
    suite.imager.width = 1.4;
    suite.exciter.warmth = 0.45;
    suite.clarity.enabled = false;

    let xml = XmlPreset::from_suite(&suite, "Roundtrip").to_xml_string();
    let parsed = XmlPreset::parse_str(&xml, "Roundtrip".to_string()).expect("parse failed");
    assert!(!parsed.plugin_ver.is_empty());

    let mut restored = MasteringSuite::new(48000.0);
    parsed.apply_to_suite(&mut restored);

    assert!((restored.maximizer.threshold_db + 7.5).abs() < 1e-4);
    assert!((restored.maximizer.ceiling_db + 0.8).abs() < 1e-4);
    assert!((restored.dynamic_eq.bands[3].static_gain_db - 2.5).abs() < 1e-4);
    assert!((restored.dynamic_eq.bands[3].freq - 900.0).abs() < 1e-3);
    assert_eq!(restored.dynamic_eq.bands[5].channel_mode, EqChannelMode::SideOnly);
    assert!((restored.multiband_dynamics.bands[2].ratio - 3.0).abs() < 1e-4);
    assert!((restored.imager.width - 1.4).abs() < 1e-4);
    assert!((restored.exciter.warmth - 0.45).abs() < 1e-4);
    assert!(!restored.clarity.enabled);
}

#[test]
fn test_render_compensates_maximizer_latency() {
    let sample_rate = 48000;
    let num_frames = 9600;
    let impulse_at = 4000;
    let mut left = vec![0.0f32; num_frames];
    left[impulse_at] = 0.25;
    let right = left.clone();
    let in_buf = AudioBuffer::from_stereo(left, right, sample_rate);

    // Only the lookahead maximizer is active, without drive, so the impulse should pass unshifted
    let mut suite = MasteringSuite::new(sample_rate as f32);
    suite.dynamic_eq.enabled = false;
    suite.multiband_dynamics.enabled = false;
    suite.imager.enabled = false;
    suite.exciter.enabled = false;
    suite.clarity.enabled = false;
    suite.maximizer.threshold_db = 0.0;
    assert!(suite.latency_samples() > 0);

    let temp_wav = std::env::temp_dir().join("test_suite_latency.wav");
    render_to_wav(&in_buf, &temp_wav, ExportBitDepth::Float32, RenderSource::Dsp(&mut suite), None, None, |_| {})
        .expect("render failed");

    let out = load_audio_file(&temp_wav).expect("decode failed");
    assert_eq!(out.num_frames(), num_frames, "latency compensation must keep the length");
    let peak_idx = out.channels[0]
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .map(|(i, _)| i)
        .unwrap();
    assert!(
        (peak_idx as i64 - impulse_at as i64).abs() <= 2,
        "impulse moved from {} to {}",
        impulse_at,
        peak_idx
    );
    let _ = std::fs::remove_file(&temp_wav);
}

#[test]
fn test_montage_album_buffer_resamples_mixed_rates() {
    let dir = std::env::temp_dir();
    let write_tone = |name: &str, sr: u32, secs: f32| {
        let n = (sr as f32 * secs) as usize;
        let l: Vec<f32> = (0..n).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        let buf = AudioBuffer::from_stereo(l.clone(), l, sr);
        let path = dir.join(name);
        render_to_wav(&buf, &path, ExportBitDepth::Float32, RenderSource::Bypass, None, None, |_| {}).unwrap();
        path
    };
    let a = write_tone("test_montage_a_44k.wav", 44100, 1.0);
    let b = write_tone("test_montage_b_48k.wav", 48000, 1.0);

    let mut montage = AlbumMontage::new("Mixed", "Tester");
    montage.add_track(a.clone(), 1.0, 44100);
    montage.add_track(b.clone(), 1.0, 48000);
    montage.tracks[1].pause_gap_seconds = 0.5;

    let album = montage.build_album_buffer(|_, _| {}).expect("build failed");
    assert_eq!(album.sample_rate, 44100);
    let expected = 44100 + 22050 + 44100;
    assert!(
        (album.num_frames() as i64 - expected as i64).abs() < 64,
        "album has {} frames, expected about {}",
        album.num_frames(),
        expected
    );

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
}

#[test]
fn test_vst3_reconfigure_and_oversized_block() {
    let Some(vst3_path) = test_vst3_path() else {
        eprintln!("MASTERING_STUDIO_TEST_VST3 not set, skipping fallback plugin test");
        return;
    };

    let plugin = Vst3Plugin::load(&vst3_path).expect("Failed to load VST3 plugin");
    // Reconfigure while active (device rate change), then feed more frames than max_block_size
    plugin.setup_audio(44100.0, 256).expect("reconfigure to 44.1 kHz failed");
    assert_eq!(plugin.max_block_size(), 256);

    let n = 1500;
    let in_l: Vec<f32> = (0..n).map(|i| (i as f32 * 0.03).sin() * 0.2).collect();
    let in_r = in_l.clone();
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    plugin.process_stereo(&in_l, &in_r, &mut out_l, &mut out_r);

    assert_eq!(plugin.sample_pos(), n as i64, "every frame should be processed across chunks");
    assert!(out_l.iter().chain(out_r.iter()).all(|s| s.is_finite()));

    plugin.setup_audio(48000.0, 2048).expect("second reconfigure failed");
}
