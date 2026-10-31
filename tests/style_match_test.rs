//! Match reference (AI). The optimiser and render loop run on every CI pass; tests that need the
//! style model skip themselves if the file is missing.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use mastering_studio::audio::ml_style::{find_model, StyleEmbedding, EMBED_DIM};
use mastering_studio::audio::{load_audio_file, AudioBuffer, AudioEngine, StyleModel};
use mastering_studio::dsp::style_match::{excerpt_starts, minimize, population_size, prepare_excerpts, CmaOptions};
use mastering_studio::dsp::{match_to_reference, MasteringSuite, MatchOptions, Module, ModuleKind, SearchSpace};

fn squared_distance(x: &[f32], target: &[f32]) -> f32 {
    x.iter().zip(target).map(|(a, b)| (a - b) * (a - b)).sum()
}

fn solve(seed: u64, target: &[f32]) -> (Vec<f32>, f32, usize) {
    let options = CmaOptions { seed, time_limit: None, ..Default::default() };
    let x0 = vec![0.5; target.len()];
    let r = minimize(&x0, &options, |c| Ok(c.iter().map(|x| squared_distance(x, target)).collect()), |_, _| true).unwrap();
    (r.best, r.best_score, r.generations)
}

#[test]
fn test_population_follows_the_plan() {
    assert_eq!(population_size(16), 12);
}

#[test]
fn test_cma_es_converges_on_a_mock_distance() {
    let target: Vec<f32> = (0..16).map(|i| 0.2 + 0.04 * i as f32).collect();
    let start = squared_distance(&[0.5; 16], &target);
    let (best, score, generations) = solve(1, &target);
    assert!(generations <= 30);
    println!("start {} best {} after {} generations", start, score, generations);
    assert!(score < start * 0.02, "score {} from {}", score, start);
    assert_eq!(squared_distance(&best, &target), score);
}

#[test]
fn test_cma_es_stays_inside_the_unit_box() {
    let target = vec![1.5f32, -0.5, 0.5, 2.0];
    let options = CmaOptions { time_limit: None, ..Default::default() };
    let mut inside = true;
    let r = minimize(
        &[0.5; 4],
        &options,
        |c| {
            inside &= c.iter().flatten().all(|v| (0.0..=1.0).contains(v));
            Ok(c.iter().map(|x| squared_distance(x, &target)).collect())
        },
        |_, _| true,
    )
    .unwrap();
    assert!(inside);
    assert!(r.best[0] > 0.8 && r.best[1] < 0.2, "{:?}", r.best);
}

#[test]
fn test_a_seed_gives_the_same_search() {
    let target: Vec<f32> = (0..12).map(|i| 0.1 + 0.07 * i as f32).collect();
    assert_eq!(solve(7, &target), solve(7, &target));
    assert_ne!(solve(7, &target).0, solve(8, &target).0);
}

#[test]
fn test_search_stops_when_it_stalls_or_is_told_to() {
    let options = CmaOptions { time_limit: None, ..Default::default() };
    let flat = minimize(&[0.5; 8], &options, |c| Ok(vec![1.0; c.len()]), |_, _| true).unwrap();
    assert_eq!(flat.generations, 6);
    let stopped = minimize(&[0.5; 8], &options, |c| Ok(c.iter().map(|x| x[0]).collect()), |g, _| g < 3).unwrap();
    assert_eq!(stopped.generations, 3);
    let failed = minimize(&[0.5; 8], &options, |_| Err("boom".to_string()), |_, _| true);
    assert_eq!(failed.unwrap_err(), "boom");
}

/// Quiet noise with a loud burst from `from` to `to` seconds
fn burst_track(seconds: f32, from: f32, to: f32) -> AudioBuffer {
    let sr = 48000;
    let n = (seconds * sr as f32) as usize;
    let mut seed: u32 = 1;
    let mut l = Vec::with_capacity(n);
    for i in 0..n {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
        let t = i as f32 / sr as f32;
        l.push(noise * if t >= from && t < to { 0.5 } else { 0.01 });
    }
    AudioBuffer::from_stereo(l.clone(), l, sr)
}

#[test]
fn test_excerpts_find_the_loud_section() {
    let track = burst_track(30.0, 12.0, 20.0);
    let starts = excerpt_starts(&track);
    assert!((2..=3).contains(&starts.len()), "{:?}", starts);
    assert!(starts.iter().all(|&s| s + 4 * 48000 <= track.num_frames()));
    let sr = 48000.0;
    assert!(starts.iter().any(|&s| (12.0..=16.5).contains(&(s as f32 / sr))), "{:?}", starts);
    assert!(starts.iter().any(|&s| !(10.0..=20.0).contains(&(s as f32 / sr))), "the median window should be quiet: {:?}", starts);
    assert_eq!(excerpt_starts(&burst_track(3.0, 1.0, 2.0)), vec![0]);
}

#[test]
fn test_rendered_excerpts_come_out_aligned_with_the_source() {
    let track = burst_track(10.0, 5.0, 8.0);
    let (src_l, _) = track.to_stereo();
    let mut suite = MasteringSuite::new(48000.0);
    for &k in ModuleKind::ALL {
        suite.module_mut(k).set_enabled(false);
    }
    // Dry chain: the excerpt is the source, whatever latency it was prepared for
    let starts = excerpt_starts(&track);
    let excerpts = prepare_excerpts(&track, None, &suite, 0);
    assert_eq!(excerpts.len(), starts.len());
    for (excerpt, &start) in excerpts.iter().zip(&starts) {
        suite.reset_state();
        let (l, _) = excerpt.render(&mut suite);
        assert_eq!(l.len(), 4 * 48000);
        assert_eq!(l.as_slice(), &src_l[start..start + l.len()]);
    }
    // Default chain: delayed by its latency, then lined up again
    let mut chain = MasteringSuite::new(48000.0);
    for &k in ModuleKind::ALL {
        chain.module_mut(k).set_enabled(k == ModuleKind::Maximizer);
    }
    chain.maximizer.set_param(0, 0.0);
    let latency = chain.latency_samples();
    assert!(latency > 0);
    let excerpts = prepare_excerpts(&track, None, &chain, latency);
    let (out, _) = excerpts[0].render(&mut chain);
    let source = &src_l[starts[0]..starts[0] + out.len()];
    let dot = |shift: usize| -> f64 { out[shift..].iter().zip(source).map(|(a, b)| (a * b) as f64).sum() };
    assert!(dot(0) > 3.0 * dot(64).abs(), "aligned {} shifted {}", dot(0), dot(64));
}

#[test]
fn test_a_match_is_one_undoable_edit_of_the_engine_suite() {
    if std::env::var_os("CI").is_some() {
        eprintln!("CI environment, skipping device test");
        return;
    }
    let Ok(engine) = AudioEngine::new() else {
        eprintln!("no audio device, skipping");
        return;
    };
    let before = engine.dsp_suite_snapshot();
    let space = SearchSpace::for_suite(&before, true).with_ceiling(-2.0);
    let x: Vec<f32> = space.encode(&before).iter().map(|v| (v + 0.2).min(1.0)).collect();
    let undo_state = engine.suite_preset();

    let (applied_space, applied) = (space.clone(), x.clone());
    engine.edit_dsp_suite(move |s| applied_space.decode(&applied, s));
    let mut expected = before.clone();
    space.decode(&x, &mut expected);
    let after = engine.dsp_suite_snapshot();
    let values = |s: &MasteringSuite| space.params.iter().map(|p| s.module(p.kind).get_param(p.index)).collect::<Vec<f32>>();
    assert_eq!(values(&after), values(&expected));
    assert_ne!(values(&after), values(&before));
    assert_eq!(after.maximizer.get_param(1), -2.0);

    engine.apply_preset(&undo_state);
    let restored = engine.dsp_suite_snapshot();
    assert_eq!(values(&restored), values(&before));
    assert_eq!(restored.maximizer.get_param(1), before.maximizer.get_param(1));
}

// ---- With the model ----

fn model() -> Option<StyleModel> {
    let Some(path) = find_model() else {
        eprintln!("models/afx_rep.onnx not found, skipping");
        return None;
    };
    Some(StyleModel::load(&path).expect("model failed to load"))
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[test]
fn test_embedding_matches_pytorch_and_ignores_gain() {
    let Some(mut model) = model() else { return };
    let track = load_audio_file("demo_mastering_track.wav").expect("demo track");
    let (l, r) = track.to_stereo();
    // The same clip the fixture was exported from (models/export_afx_rep.py)
    let (start, end) = (63999, 63999 + 4 * 48000);
    let (l, r) = (&l[start..end], &r[start..end]);
    let started = std::time::Instant::now();
    let e = model.embed(l, r, track.sample_rate).expect("embed failed");
    println!("4 s clip embedded in {:.3} s", started.elapsed().as_secs_f32());

    let bytes = std::fs::read("tests/fixtures/afx_rep_demo_embedding.bin").expect("fixture");
    let floats: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
    let unit = |v: &[f32]| -> Vec<f32> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect()
    };
    let (mid, side) = floats.split_at(EMBED_DIM);
    let (cos_mid, cos_side) = (cosine(&e.mid, &unit(mid)), cosine(&e.side, &unit(side)));
    println!("parity with PyTorch: mid {:.6}, side {:.6}", cos_mid, cos_side);
    assert!(cos_mid > 0.999 && cos_side > 0.999, "mid {} side {}", cos_mid, cos_side);

    let quiet = |x: &[f32]| x.iter().map(|v| v * 0.25).collect::<Vec<f32>>();
    let e2 = model.embed(&quiet(l), &quiet(r), track.sample_rate).unwrap();
    assert!(e.distance(&e2) < 1e-4, "gain moved the embedding by {}", e.distance(&e2));

    // A different rate reaches the model at 48 kHz
    let resampled = mastering_studio::audio::SincResampler::default().resample_stereo(l, r, 48000, 44100);
    let e3 = model.embed(&resampled.0, &resampled.1, 44100).unwrap();
    assert!(e.distance(&e3) < 0.01, "resampling moved the embedding by {}", e.distance(&e3));

    assert!(StyleEmbedding::average(&[]).is_none());
    assert!(model.embed(&[0.0; 1000], &[0.0; 1000], 48000).is_err());
}

/// The demo track with known EQ, compression and width, standing in for a reference master
fn fake_reference(track: &AudioBuffer, suite: &MasteringSuite) -> AudioBuffer {
    let mut suite = suite.clone();
    suite.set_sample_rate(track.sample_rate as f32);
    suite.reset_state();
    let (mut l, mut r) = track.to_stereo();
    for (a, b) in l.chunks_mut(1024).zip(r.chunks_mut(1024)) {
        suite.process_stereo(a, b);
    }
    AudioBuffer::from_stereo(l, r, track.sample_rate)
}

#[test]
fn test_match_recovers_known_settings() {
    if cfg!(debug_assertions) {
        eprintln!("needs the full search: run with --release");
        return;
    }
    let Some(mut model) = model() else { return };
    let track = load_audio_file("demo_mastering_track.wav").expect("demo track");
    let start = MasteringSuite::new(track.sample_rate as f32);

    let space = SearchSpace::for_suite(&start, true);
    let gain = |band: usize| band * 9 + 3;
    let (low, high) = (gain(1), gain(6));
    let mut known = start.clone();
    known.dynamic_eq.set_param(low, 6.0);
    known.dynamic_eq.set_param(high, -5.0);
    let reference = fake_reference(&track, &known);

    let mut options = MatchOptions::default();
    options.search.time_limit = None;
    let started = std::time::Instant::now();
    let result = match_to_reference(&track, &reference, &start, &mut model, None, &options, |p| {
        println!("gen {}/{}: {:.1}% closer", p.generation, p.generations, p.closer_percent);
    })
    .expect("match failed");
    println!("{:?}\n{}", started.elapsed(), result.to_text());

    assert!(result.improved());
    assert!(result.improvement_percent() >= 50.0, "only {:.1}% closer", result.improvement_percent());
    let mut matched = start.clone();
    result.space.decode(&result.best, &mut matched);
    for index in [low, high] {
        let (want, got) = (known.dynamic_eq.get_param(index), matched.dynamic_eq.get_param(index));
        println!("{}: wanted {:+.1} dB, got {:+.1} dB", space.params.iter().find(|p| p.kind == ModuleKind::Equalizer && p.index == index).unwrap().label(), want, got);
        assert!((want - got).abs() <= 2.0, "wanted {} got {}", want, got);
    }
}

#[test]
fn test_match_is_repeatable_cancellable_and_leaves_the_suite_alone() {
    let Some(mut model) = model() else { return };
    let track = load_audio_file("demo_mastering_track.wav").expect("demo track");
    let start = MasteringSuite::new(track.sample_rate as f32);
    let mut other = start.clone();
    other.imager.set_param(0, 0.7);
    let reference = fake_reference(&track, &other);

    let mut options = MatchOptions::default();
    options.search.generations = 2;
    options.search.time_limit = None;
    let a = match_to_reference(&track, &reference, &start, &mut model, None, &options, |_| {}).unwrap();
    let b = match_to_reference(&track, &reference, &start, &mut model, None, &options, |_| {}).unwrap();
    assert_eq!(a.best, b.best);
    assert_eq!(a.best_distance, b.best_distance);
    assert_eq!(a.evaluations, 2 * population_size(a.space.len()));

    options.cancel = Some(Arc::new(AtomicBool::new(true)));
    let cancelled = match_to_reference(&track, &reference, &start, &mut model, None, &options, |_| {});
    assert_eq!(cancelled.unwrap_err(), "Cancelled");
}

#[test]
fn test_resampler_is_flat_across_the_band_at_any_ratio() {
    let resampler = mastering_studio::audio::SincResampler::default();
    let rms = |x: &[f32]| {
        let middle = &x[x.len() / 4..3 * x.len() / 4];
        (middle.iter().map(|v| v * v).sum::<f32>() / middle.len() as f32).sqrt()
    };
    for (from, to) in [(44100u32, 48000u32), (48000, 44100), (96000, 44100), (44100, 96000)] {
        for hz in [100.0f32, 1000.0, 5000.0, 10000.0, 15000.0, 18000.0] {
            let x: Vec<f32> = (0..from as usize / 4)
                .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / from as f32).sin() * 0.5)
                .collect();
            let (y, _) = resampler.resample_stereo(&x, &x, from, to);
            let db = 20.0 * (rms(&y) / rms(&x)).log10();
            // The filter starts rolling off near the top of the band on steep conversions
            let tolerance = if hz > 15000.0 { 0.75 } else { 0.2 };
            assert!(db.abs() < tolerance, "{} to {} at {} Hz: {:+.2} dB", from, to, hz, db);
        }
    }
}
