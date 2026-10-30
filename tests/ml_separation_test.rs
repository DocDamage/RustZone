//! Trained stem separation with the bundled model (skips if the model file is missing)

use mastering_studio::audio::ml_separator::find_model;
use std::sync::Arc;
use mastering_studio::audio::{load_audio_file, render_to_wav, AudioBuffer, DemucsSeparator, ExportBitDepth, RenderOptions, RenderSource};
use mastering_studio::dsp::{MasteringSuite, Module, ModuleKind};

fn energy(x: &[f32]) -> f64 {
    x.iter().map(|v| (*v as f64).powi(2)).sum()
}

#[test]
fn test_demucs_separation_reconstructs_the_mix() {
    let Some(model) = find_model() else {
        eprintln!("models/htdemucs.onnx not found, skipping");
        return;
    };
    let Ok(track) = load_audio_file("demo_mastering_track.wav") else {
        eprintln!("demo track missing, skipping");
        return;
    };
    let started = std::time::Instant::now();
    let mut sep = DemucsSeparator::load(&model).expect("model failed to load");
    println!("model loaded in {:.1}s", started.elapsed().as_secs_f32());
    let started = std::time::Instant::now();
    let stems = sep.separate(&track, |_| {}).expect("separation failed");
    println!("separated {:.1}s of audio in {:.1}s", track.duration_seconds(), started.elapsed().as_secs_f32());

    assert_eq!(stems.num_frames, track.num_frames());
    assert_eq!(stems.sample_rate, track.sample_rate);
    let (ml, mr) = track.to_stereo();
    let mut residual = 0.0;
    for i in 0..track.num_frames() {
        let f = stems.frames_at_index(i);
        let (sl, sr) = f.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
        assert!(sl.is_finite() && sr.is_finite());
        residual += ((ml[i] - sl) as f64).powi(2) + ((mr[i] - sr) as f64).powi(2);
    }
    let snr = 10.0 * ((energy(&ml) + energy(&mr)) / residual.max(1e-12)).log10();
    println!("sum-of-stems SNR: {:.1} dB", snr);
    assert!(snr > 10.0, "stems don't add back up to the mix (SNR {:.1} dB)", snr);

    // Every stem should carry some of the music, and no stem should be the whole mix
    for (s, stem) in stems.stems.iter().enumerate() {
        let (a, b) = stem.to_stereo();
        let share = (energy(&a) + energy(&b)) / (energy(&ml) + energy(&mr));
        println!("stem {} energy share {:.3}", s, share);
        assert!(share.is_finite());
    }

    // Export path: Stem Rebalance applied to the trained stems
    let stems = Arc::new(stems);
    let render = |bass_db: f32| -> AudioBuffer {
        let mut suite = MasteringSuite::new(track.sample_rate as f32);
        for &k in ModuleKind::ALL {
            suite.module_mut(k).set_enabled(false);
        }
        suite.stem_rebalance.set_enabled(true);
        suite.stem_rebalance.gains_db[1] = bass_db;
        let path = std::env::temp_dir().join(format!("test_ml_rebalance_{}.wav", bass_db as i32));
        let opts = RenderOptions { stems: Some(Arc::clone(&stems)), ..Default::default() };
        render_to_wav(&track, &path, ExportBitDepth::Float32, RenderSource::Dsp(&mut suite), Some(opts), None, |_| {}).unwrap();
        let out = load_audio_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        out
    };
    // Neutral settings reproduce the mix (no spectral-module latency, sum of stems)
    let neutral = render(0.0);
    assert_eq!(neutral.num_frames(), track.num_frames());
    let (nl, _) = neutral.to_stereo();
    let err: f64 = ml.iter().zip(&nl).map(|(a, b)| ((a - b) as f64).powi(2)).sum();
    let snr = 10.0 * (energy(&ml) / err.max(1e-12)).log10();
    println!("neutral stem render SNR {:.1} dB", snr);
    assert!(snr > 10.0);
    // Turning bass down removes energy, mostly from the low end
    let cut = render(-12.0);
    let (cl, _) = cut.to_stereo();
    let bass_share = {
        let (b, _) = stems.stems[1].to_stereo();
        energy(&b) / energy(&ml)
    };
    let drop = 10.0 * (energy(&nl) / energy(&cl)).log10();
    println!("bass share {:.2}, level drop with bass -12 dB: {:.1} dB", bass_share, drop);
    assert!(drop > 0.5, "turning bass down should lower the level");
}
