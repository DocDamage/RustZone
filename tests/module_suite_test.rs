//! Behavioural tests for the built-in mastering modules

use mastering_studio::audio::{load_audio_file, render_to_wav, AudioBuffer, ExportBitDepth, RenderSource};
use mastering_studio::dsp::stft::{StftProcessor, STFT_SIZE};
use mastering_studio::dsp::{MasteringSuite, Module, ModuleKind, XmlPreset};

const SR: f32 = 48000.0;

/// Deterministic pseudo-random noise in [-amp, amp]
fn noise(n: usize, seed: u64, amp: f32) -> Vec<f32> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((s >> 33) as f32 / (1u64 << 31) as f32 * 2.0 - 1.0) * amp
        })
        .collect()
}

fn sine(n: usize, hz: f32, amp: f32) -> Vec<f32> {
    (0..n).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / SR).sin() * amp).collect()
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// Level of a single frequency (Goertzel)
fn tone_level(x: &[f32], hz: f32) -> f32 {
    let w = 2.0 * std::f32::consts::PI * hz / SR;
    let c = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &v in x {
        let s0 = v + c * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - c * s1 * s2).max(0.0).sqrt() / x.len() as f32
}

/// First difference RMS: a crude measure of high-frequency content
fn hf_energy(x: &[f32]) -> f32 {
    rms(&x.windows(2).map(|w| w[1] - w[0]).collect::<Vec<_>>())
}

/// Suite with every module switched off
fn bare_suite() -> MasteringSuite {
    let mut suite = MasteringSuite::new(SR);
    for &k in ModuleKind::ALL {
        suite.module_mut(k).set_enabled(false);
    }
    suite
}

fn run_module(kind: ModuleKind, setup: impl FnOnce(&mut MasteringSuite), l: &[f32], r: &[f32]) -> (Vec<f32>, Vec<f32>, usize) {
    let mut suite = bare_suite();
    suite.module_mut(kind).set_enabled(true);
    setup(&mut suite);
    let (mut ol, mut or) = (l.to_vec(), r.to_vec());
    for (cl, cr) in ol.chunks_mut(480).zip(or.chunks_mut(480)) {
        suite.process_stereo(cl, cr);
    }
    let lat = suite.latency_samples();
    (ol, or, lat)
}

#[test]
fn test_stft_is_transparent_with_one_frame_latency() {
    let n = STFT_SIZE * 8;
    let input = noise(n, 7, 0.5);
    let (mut l, mut r) = (input.clone(), input.clone());
    let mut stft = StftProcessor::new();
    stft.process(&mut l, &mut r, &mut |_| {});
    let lat = stft.latency();
    let err = (STFT_SIZE * 2..n).map(|i| (l[i] - input[i - lat]).abs()).fold(0.0f32, f32::max);
    assert!(err < 1e-4, "STFT round trip error {}", err);
}

#[test]
fn test_every_module_is_stable_on_program_material() {
    let n = (SR * 2.0) as usize;
    let mut l = noise(n, 1, 0.3);
    let mut r = noise(n, 2, 0.3);
    let bass = sine(n, 55.0, 0.4);
    let lead = sine(n, 440.0, 0.2);
    for i in 0..n {
        l[i] += bass[i] + lead[i];
        r[i] += bass[i] - lead[i];
    }
    for &kind in ModuleKind::ALL {
        let (ol, or, _) = run_module(kind, |_| {}, &l, &r);
        let peak = ol.iter().chain(or.iter()).fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(ol.iter().chain(or.iter()).all(|v| v.is_finite()), "{:?} produced NaN/Inf", kind);
        assert!(peak < 4.0, "{:?} blew up (peak {})", kind, peak);
    }
}

#[test]
fn test_neutral_spectral_modules_are_transparent() {
    let n = STFT_SIZE * 12;
    let l = noise(n, 3, 0.4);
    let r = noise(n, 4, 0.4);
    for kind in [ModuleKind::StemRebalance, ModuleKind::StemEq, ModuleKind::MatchEq] {
        let (ol, or, lat) = run_module(kind, |_| {}, &l, &r);
        assert_eq!(lat, STFT_SIZE);
        let err = (STFT_SIZE * 2..n)
            .map(|i| (ol[i] - l[i - lat]).abs().max((or[i] - r[i - lat]).abs()))
            .fold(0.0f32, f32::max);
        assert!(err < 1e-3, "{:?} not transparent at neutral settings (err {})", kind, err);
    }
}

#[test]
fn test_stem_rebalance_turns_bass_down() {
    let n = (SR * 3.0) as usize;
    let bass = sine(n, 60.0, 0.4);
    let hats = noise(n, 9, 0.05);
    let l: Vec<f32> = bass.iter().zip(&hats).map(|(a, b)| a + b).collect();
    let r = l.clone();
    let before = tone_level(&l[n / 2..], 60.0);
    let (ol, _, _) = run_module(ModuleKind::StemRebalance, |s| s.stem_rebalance.gains_db[1] = -12.0, &l, &r);
    let after = tone_level(&ol[n / 2..], 60.0);
    let change_db = 20.0 * (after / before).log10();
    assert!(change_db < -6.0, "bass only changed by {:.1} dB", change_db);
}

#[test]
fn test_match_eq_learns_a_darker_reference() {
    let n = (SR * 4.0) as usize;
    let bright = noise(n, 11, 0.3);
    // One-pole low-pass makes the reference darker
    let mut dark = vec![0.0f32; n];
    let mut y = 0.0;
    for i in 0..n {
        y += 0.15 * (bright[i] - y);
        dark[i] = y * 2.0;
    }
    let src = AudioBuffer::from_stereo(bright.clone(), bright.clone(), SR as u32);
    let refr = AudioBuffer::from_stereo(dark.clone(), dark, SR as u32);

    let mut suite = bare_suite();
    suite.match_eq.learn(&src, &refr).expect("learn failed");
    suite.match_eq.set_enabled(true);
    suite.match_eq.set_param(0, 1.0);
    let curve = suite.match_eq.curve_db();
    let low = curve[24 * 2]; // ~80 Hz
    let high = curve[curve.len() - 24]; // ~10 kHz
    assert!(high < low - 3.0, "curve should cut highs: low {:.1} dB, high {:.1} dB", low, high);

    let (mut l, mut r) = (bright.clone(), bright.clone());
    suite.process_stereo(&mut l, &mut r);
    assert!(hf_energy(&l[n / 2..]) < hf_energy(&bright[n / 2..]) * 0.8, "high frequencies were not reduced");
}

#[test]
fn test_transient_restore_increases_crest_factor() {
    // Squashed drum-like bursts: instant attack, flat sustain
    let n = (SR * 2.0) as usize;
    let l: Vec<f32> = (0..n)
        .map(|i| {
            let env = if i % 12000 < 4000 { 0.5 } else { 0.0 };
            env * (i as f32 * 0.3).sin()
        })
        .collect();
    let r = l.clone();
    let crest = |x: &[f32]| x.iter().fold(0.0f32, |m, v| m.max(v.abs())) / rms(x);
    let (ol, _, _) = run_module(ModuleKind::TransientRestore, |s| s.transient_restore.amount = 1.0, &l, &r);
    assert!(crest(&ol) > crest(&l) * 1.05, "crest {:.2} -> {:.2}", crest(&l), crest(&ol));
}

#[test]
fn test_opto_limiter_respects_ceiling() {
    let n = SR as usize;
    let l = sine(n, 200.0, 0.95);
    let r = l.clone();
    let (ol, or, _) = run_module(
        ModuleKind::OptoLimiter,
        |s| {
            s.opto_limiter.drive_db = 12.0;
            s.opto_limiter.ceiling_db = -1.0;
        },
        &l,
        &r,
    );
    let ceiling = 10.0f32.powf(-1.0 / 20.0);
    assert!(ol.iter().chain(or.iter()).all(|v| v.abs() <= ceiling + 1e-6));
}

#[test]
fn test_render_compensates_spectral_latency() {
    let n = 24000;
    let at = 9000;
    let mut l = vec![0.0f32; n];
    l[at] = 0.5;
    let buf = AudioBuffer::from_stereo(l.clone(), l, SR as u32);
    let mut suite = bare_suite();
    suite.stem_eq.set_enabled(true);
    assert_eq!(suite.latency_samples(), STFT_SIZE);

    let path = std::env::temp_dir().join("test_suite_spectral_latency.wav");
    render_to_wav(&buf, &path, ExportBitDepth::Float32, RenderSource::Dsp(&mut suite), None, None, |_| {}).unwrap();
    let out = load_audio_file(&path).unwrap();
    assert_eq!(out.num_frames(), n);
    let peak = out.channels[0]
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .unwrap()
        .0;
    assert!((peak as i64 - at as i64).abs() <= 1, "impulse moved from {} to {}", at, peak);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_full_chain_preset_roundtrip() {
    let mut suite = MasteringSuite::new(SR);
    suite.move_module(ModuleKind::Maximizer, -5);
    suite.move_module(ModuleKind::MatchEq, 3);
    suite.tape.set_enabled(true);
    suite.tape.set_param(0, 7.5);
    suite.stem_eq.set_param(4, -3.0); // Bass mid
    suite.vintage_comp.set_param(7, 2.0); // Vari-Mu
    suite.imager.band_width[3] = 1.6;
    suite.stabilizer.set_enabled(true);

    let xml = XmlPreset::from_suite(&suite, "Full").to_xml_string();
    let parsed = XmlPreset::parse_str(&xml, "Full".to_string()).unwrap();
    let mut restored = MasteringSuite::new(SR);
    parsed.apply_to_suite(&mut restored);

    assert_eq!(restored.chain, suite.chain);
    for &k in ModuleKind::ALL {
        let a = suite.module(k);
        let b = restored.module(k);
        assert_eq!(a.enabled(), b.enabled(), "{:?} on/off", k);
        for i in 0..a.params().len() {
            assert!((a.get_param(i) - b.get_param(i)).abs() < 1e-3, "{:?} param {} differs", k, a.params()[i].name);
        }
    }
}

/// Neighbouring spectral modules share one STFT: one FFT length of latency, and the same sound
/// as running each module through its own STFT in turn
#[test]
fn test_neighbouring_spectral_modules_share_one_pass() {
    let n = STFT_SIZE * 48;
    let mut l = noise(n, 11, 0.3);
    let mut r = noise(n, 12, 0.3);
    let bass = sine(n, 70.0, 0.3);
    let lead = sine(n, 3000.0, 0.15);
    for i in 0..n {
        l[i] += bass[i] + lead[i];
        r[i] += bass[i] + 0.5 * lead[i];
    }
    let configure = |s: &mut MasteringSuite| {
        s.stem_rebalance.set_enabled(true);
        s.stem_rebalance.gains_db = [3.0, -4.0, 2.0, 0.0];
        s.stem_eq.set_enabled(true);
        s.stem_eq.set_param(5, 3.0); // Bass high
        s.spectral_shaper.set_enabled(true);
        s.spectral_shaper.set_param(2, 2.0);
        s.stabilizer.set_enabled(true);
        s.stabilizer.set_param(0, 0.8);
        // The Stabilizer primes on its first non-silent frame, which differs when it sits behind
        // its own STFT; a fast speed lets that start-up difference die away within the test
        s.stabilizer.set_param(1, 200.0);
    };

    let mut suite = bare_suite();
    configure(&mut suite);
    // Only switched-off modules lie between them, so all four form one pass
    assert_eq!(suite.latency_samples(), STFT_SIZE);
    assert_eq!(suite.shared_spectral_modules(), 4);
    let (mut sl, mut sr) = (l.clone(), r.clone());
    for (cl, cr) in sl.chunks_mut(480).zip(sr.chunks_mut(480)) {
        suite.process_stereo(cl, cr);
    }

    // The same modules, each through its own STFT, in chain order
    let mut separate = bare_suite();
    configure(&mut separate);
    let (mut ol, mut or) = (l.clone(), r.clone());
    for kind in [ModuleKind::StemRebalance, ModuleKind::StemEq, ModuleKind::SpectralShaper, ModuleKind::Stabilizer] {
        for (cl, cr) in ol.chunks_mut(480).zip(or.chunks_mut(480)) {
            separate.module_mut(kind).process(cl, cr);
        }
    }
    let saved = 3 * STFT_SIZE; // separate processing has four times the latency
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for i in n / 2..n {
        for (a, b) in [(sl[i - saved], ol[i]), (sr[i - saved], or[i])] {
            sig += (b as f64).powi(2);
            err += ((a - b) as f64).powi(2);
        }
    }
    let snr = 10.0 * (sig / err.max(1e-30)).log10();
    println!("shared vs separate spectral processing: SNR {:.1} dB", snr);
    assert!(snr > 30.0, "shared pass sounds different from separate modules (SNR {:.1} dB)", snr);

    // Auto gain-match measures a module's own input and output, so it takes the module out of the pass
    suite.set_auto_gain(ModuleKind::StemEq, true);
    assert_eq!(suite.latency_samples(), 3 * STFT_SIZE);
    assert_eq!(suite.shared_spectral_modules(), 2);
    // A time-domain module in between splits the pass in two
    suite.set_auto_gain(ModuleKind::StemEq, false);
    suite.imager.enabled = true;
    assert_eq!(suite.latency_samples(), 2 * STFT_SIZE);
    assert_eq!(suite.shared_spectral_modules(), 4);
}
