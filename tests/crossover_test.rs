//! Multiband crossovers must recombine to a flat magnitude response

use mastering_studio::audio::Radix2Fft;
use mastering_studio::dsp::{Crossover4, MasteringSuite, ModuleKind};

const SR: f32 = 48000.0;
const N: usize = 16384;

/// Magnitude response (dB) of an impulse response at bins between 20 Hz and 20 kHz: (min, max)
fn response_range(ir: &[f32]) -> (f32, f32) {
    let fft = Radix2Fft::new(N);
    let mut re = ir.to_vec();
    re.resize(N, 0.0);
    let mut im = vec![0.0; N];
    fft.forward(&mut re, &mut im);
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for k in 1..N / 2 {
        let f = k as f32 * SR / N as f32;
        if (20.0..=20000.0).contains(&f) {
            let db = 10.0 * (re[k] * re[k] + im[k] * im[k]).max(1e-20).log10();
            lo = lo.min(db);
            hi = hi.max(db);
        }
    }
    (lo, hi)
}

#[test]
fn test_crossover_bands_sum_flat() {
    let mut xo = Crossover4::new([140.0, 1500.0, 6000.0], SR);
    let ir: Vec<f32> = (0..N)
        .map(|i| {
            let x = if i == 0 { 1.0 } else { 0.0 };
            xo.split(x, x).iter().map(|b| b.0).sum()
        })
        .collect();
    let (lo, hi) = response_range(&ir);
    println!("crossover sum: {:+.3} .. {:+.3} dB", lo, hi);
    assert!(lo > -0.05 && hi < 0.05, "crossover sum not flat: {:.3}..{:.3} dB", lo, hi);
}

#[test]
fn test_neutral_multiband_modules_are_flat() {
    for kind in [ModuleKind::Dynamics, ModuleKind::Imager, ModuleKind::Transient] {
        let mut suite = MasteringSuite::new(SR);
        for &k in ModuleKind::ALL {
            suite.module_mut(k).set_enabled(false);
        }
        suite.module_mut(kind).set_enabled(true);
        match kind {
            ModuleKind::Dynamics => {
                for b in suite.multiband_dynamics.bands.iter_mut() {
                    b.ratio = 1.0;
                    b.makeup_gain_db = 0.0;
                }
            }
            ModuleKind::Imager => {
                // Force the multiband path with a tiny width change on one band; mono input is unaffected
                suite.imager.band_width = [1.0, 1.0001, 1.0, 1.0];
            }
            _ => {
                // Neutral transient shaping, but not skipped: a tiny punch so the bands run
                suite.transient.punch = [1e-3, 0.0, 0.0, 0.0];
            }
        }
        let ir: Vec<f32> = (0..N)
            .map(|i| {
                let (mut l, mut r) = ([if i == 0 { 0.5 } else { 0.0 }], [if i == 0 { 0.5 } else { 0.0 }]);
                suite.process_stereo(&mut l, &mut r);
                l[0] * 2.0
            })
            .collect();
        let (lo, hi) = response_range(&ir);
        println!("{:?}: {:+.3} .. {:+.3} dB", kind, lo, hi);
        assert!(lo > -0.25 && hi < 0.25, "{:?} not flat when neutral: {:.3}..{:.3} dB", kind, lo, hi);
    }
}
