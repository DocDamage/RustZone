//! Oversampling: transparency, exact latency, aliasing reduction, and alignment in renders

use mastering_studio::audio::{load_audio_file, render_to_wav, AudioBuffer, ExportBitDepth, RenderSource};
use mastering_studio::dsp::oversampling::Oversampler;
use mastering_studio::dsp::{MasteringSuite, Module, ModuleKind};

const SR: f32 = 48000.0;

fn sine(n: usize, hz: f32, amp: f32) -> Vec<f32> {
    (0..n).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / SR).sin() * amp).collect()
}

/// Level of one frequency (Goertzel), normalised to the sine amplitude
fn tone_level(x: &[f32], hz: f32) -> f32 {
    let w = 2.0 * std::f32::consts::PI * hz / SR;
    let c = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s0 = v as f64 + c as f64 * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    ((s1 * s1 + s2 * s2 - c as f64 * s1 * s2).max(0.0).sqrt() * 2.0 / x.len() as f64) as f32
}

fn process_in_blocks(os: &mut Oversampler, l: &mut [f32], r: &mut [f32], f: &mut dyn FnMut(&mut [f32], &mut [f32])) {
    for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
        os.process(cl, cr, f);
    }
}

#[test]
fn test_oversampler_is_transparent_with_whole_sample_latency() {
    for (factor, expected_latency) in [(2, 24), (4, 36)] {
        let mut os = Oversampler::new(factor);
        assert_eq!(os.latency(), expected_latency);
        let n = 8192;
        // Band-limited test signal well below the original Nyquist
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / SR;
                0.4 * (2.0 * std::f32::consts::PI * 997.0 * t).sin() + 0.3 * (2.0 * std::f32::consts::PI * 12_345.0 * t).sin()
            })
            .collect();
        let (mut l, mut r) = (input.clone(), input.clone());
        process_in_blocks(&mut os, &mut l, &mut r, &mut |_, _| {});
        let lat = os.latency();
        let err = (1000..n).map(|i| (l[i] - input[i - lat]).abs()).fold(0.0f32, f32::max);
        assert!(err < 2e-3, "{}x round trip error {}", factor, err);
    }
}

#[test]
fn test_oversampling_reduces_clipping_aliases() {
    let n = 48000;
    let f0 = 9_700.0;
    // 3rd harmonic (29.1 kHz) aliases to 18.9 kHz at 48 kHz; 5th (48.5 kHz) to 0.5 kHz
    let aliases = [48_000.0 - 3.0 * f0, 5.0 * f0 - 48_000.0];
    let clip = |l: &mut [f32], r: &mut [f32]| {
        for v in l.iter_mut().chain(r.iter_mut()) {
            *v = (*v * 4.0).clamp(-0.5, 0.5);
        }
    };
    let alias_level = |factor: usize| -> f32 {
        let mut os = Oversampler::new(factor);
        let (mut l, mut r) = (sine(n, f0, 0.8), sine(n, f0, 0.8));
        process_in_blocks(&mut os, &mut l, &mut r, &mut |a, b| clip(a, b));
        aliases.iter().map(|&f| tone_level(&l[4096..], f)).fold(0.0, f32::max)
    };
    let base = alias_level(1);
    let x2 = alias_level(2);
    let x4 = alias_level(4);
    let db = |v: f32| 20.0 * v.max(1e-9).log10();
    println!("alias level: 1x {:.1} dB, 2x {:.1} dB, 4x {:.1} dB", db(base), db(x2), db(x4));
    assert!(db(base) - db(x2) > 20.0, "2x should cut aliases by over 20 dB");
    assert!(db(x4) <= db(x2) + 1.0);
}

#[test]
fn test_render_stays_aligned_with_oversampled_modules() {
    let n = 24000;
    let at = 9000;
    let mut l = vec![0.0f32; n];
    l[at] = 0.25;
    let buf = AudioBuffer::from_stereo(l.clone(), l, SR as u32);
    let mut suite = MasteringSuite::new(SR);
    for &k in ModuleKind::ALL {
        suite.module_mut(k).set_enabled(false);
    }
    suite.tape.set_enabled(true);
    suite.tape.set_param(0, 0.0); // no drive: nearly linear, so the impulse survives
    suite.set_oversampling(ModuleKind::Tape, 4);
    suite.passive_eq.set_enabled(true);
    suite.set_oversampling(ModuleKind::PassiveEq, 2);
    assert_eq!(suite.latency_samples(), 36 + 24);

    let path = std::env::temp_dir().join("test_oversampled_alignment.wav");
    render_to_wav(&buf, &path, ExportBitDepth::Float32, RenderSource::Dsp(&mut suite), None, None, |_| {}).unwrap();
    let out = load_audio_file(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(out.num_frames(), n);
    let peak = out.channels[0]
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .unwrap()
        .0;
    assert!((peak as i64 - at as i64).abs() <= 1, "impulse moved from {} to {}", at, peak);
}

#[test]
fn test_saturators_default_to_2x_and_presets_keep_the_setting() {
    let mut suite = MasteringSuite::new(SR);
    assert_eq!(suite.oversampling[ModuleKind::Exciter.index()], 2);
    assert_eq!(suite.oversampling[ModuleKind::Equalizer.index()], 1);
    assert!(!suite.supports_oversampling(ModuleKind::StemEq));
    suite.set_oversampling(ModuleKind::StemEq, 4);
    assert_eq!(suite.oversampling[ModuleKind::StemEq.index()], 1, "spectral modules don't oversample");

    suite.set_oversampling(ModuleKind::Exciter, 4);
    suite.set_oversampling(ModuleKind::Tape, 1);
    let xml = mastering_studio::dsp::XmlPreset::from_suite(&suite, "os").to_xml_string();
    let mut restored = MasteringSuite::new(SR);
    mastering_studio::dsp::XmlPreset::parse_str(&xml, "os".into()).unwrap().apply_to_suite(&mut restored);
    assert_eq!(restored.oversampling[ModuleKind::Exciter.index()], 4);
    assert_eq!(restored.oversampling[ModuleKind::Tape.index()], 1);
}
