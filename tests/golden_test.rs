//! Golden-audio regression tests: fixed input through fixed chains must match stored output.
//!
//! After an intended change to the sound, re-record with `UPDATE_GOLDEN=1 cargo test --test golden_test`
//! and commit the updated files in tests/golden/.

use std::path::PathBuf;

use mastering_studio::dsp::{MasteringSuite, Module, ModuleKind};

const SR: f32 = 48000.0;
const SECONDS: f32 = 1.0;
/// Maximum allowed sample difference (about -80 dBFS)
const TOLERANCE: f32 = 1e-4;

/// Deterministic programme-like stereo signal: pulsed kick, bass, chords, hats and noise
fn test_signal() -> (Vec<f32>, Vec<f32>) {
    let n = (SR * SECONDS) as usize;
    let mut seed: u64 = 0x5EED;
    let mut noise = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as f32 / (1u64 << 31) as f32 * 2.0 - 1.0
    };
    let tau = 2.0 * std::f32::consts::PI;
    let mut l = Vec::with_capacity(n);
    let mut r = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / SR;
        let beat = t % 0.5;
        let kick = (tau * (50.0 + 120.0 * (-beat * 30.0).exp()) * beat).sin() * (-beat * 9.0).exp() * 0.6;
        let bass = (tau * 55.0 * t).sin() * 0.25;
        let chord = [220.0, 277.2, 329.6].iter().map(|f| (tau * f * t).sin()).sum::<f32>() * 0.08;
        let hat_env = (-(t % 0.125) * 80.0).exp();
        let hats = noise() * hat_env * 0.12;
        let air = noise() * 0.01;
        let width = (tau * 0.5 * t).sin() * 0.04;
        l.push(kick + bass + chord + hats + air + width);
        r.push(kick + bass + chord * 0.9 + hats * 0.7 + air - width);
    }
    (l, r)
}

fn run(suite: &mut MasteringSuite) -> Vec<f32> {
    let (mut l, mut r) = test_signal();
    for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
        suite.process_stereo(cl, cr);
    }
    l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect()
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden").join(format!("{}.f32le", name))
}

fn check(name: &str, out: &[f32]) {
    let path = golden_path(name);
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    if update || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes: Vec<u8> = out.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(&path, bytes).unwrap();
        if !update {
            panic!("golden file {} was missing and has been recorded; re-run to compare", path.display());
        }
        return;
    }
    let bytes = std::fs::read(&path).unwrap();
    let golden: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
    assert_eq!(golden.len(), out.len(), "{}: length changed", name);
    let mut max_diff = 0.0f32;
    let mut worst = 0;
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for (i, (a, b)) in out.iter().zip(&golden).enumerate() {
        assert!(a.is_finite(), "{}: non-finite sample at {}", name, i);
        let d = (a - b).abs();
        if d > max_diff {
            max_diff = d;
            worst = i;
        }
        sig += (*b as f64).powi(2);
        err += (d as f64).powi(2);
    }
    let snr = 10.0 * (sig / err.max(1e-30)).log10();
    println!("{}: max diff {:.2e} at frame {}, SNR vs golden {:.1} dB", name, max_diff, worst / 2, snr);
    assert!(
        max_diff <= TOLERANCE,
        "{}: output differs from golden (max diff {:.2e} at frame {}, SNR {:.1} dB). If the change is intended, run with UPDATE_GOLDEN=1",
        name,
        max_diff,
        worst / 2,
        snr
    );
}

#[test]
fn golden_default_chain() {
    let mut suite = MasteringSuite::new(SR);
    check("default_chain", &run(&mut suite));
}

#[test]
fn golden_full_chain() {
    let mut suite = MasteringSuite::new(SR);
    for &k in ModuleKind::ALL {
        suite.module_mut(k).set_enabled(true);
    }
    // Move every module off its neutral point so each one shapes the result
    suite.passive_eq.set_param(1, 3.0);
    suite.passive_eq.set_param(4, 2.0);
    suite.stem_rebalance.gains_db = [2.0, -2.0, 1.0, 0.0];
    suite.stem_eq.set_param(2, 2.0);
    suite.low_end.set_param(1, 0.4);
    suite.bass.set_param(1, 2.0);
    suite.bass.set_param(3, 0.3);
    suite.vintage_comp.set_param(0, -24.0);
    suite.transient.punch = [0.3, 0.0, -0.2, 0.2];
    suite.transient_restore.set_param(0, 0.4);
    suite.imager.band_width = [0.0, 1.0, 1.2, 1.3];
    suite.imager.stereoize = 0.2;
    suite.tape.set_param(0, 6.0);
    suite.spectral_shaper.set_param(2, 3.0);
    suite.stabilizer.set_param(0, 0.6);
    suite.opto_limiter.set_param(0, 4.0);
    suite.set_auto_gain(ModuleKind::Exciter, true);
    check("full_chain", &run(&mut suite));
}
