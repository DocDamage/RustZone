//! Text requests. The prompt, grammar and limits run on every CI pass with a stand-in for the
//! model; tests that need the real one skip themselves if it hasn't been downloaded.

use std::sync::atomic::AtomicBool;

use mastering_studio::audio::{ml_text, AudioBuffer, TextModel};
use mastering_studio::dsp::text_edit::{grammar, parse_answer, spectral_shape, system_prompt, MAX_EDITS, MAX_TOKENS};
use mastering_studio::dsp::{plan_request, MasteringSuite, Module, ModuleKind, TextPlan};

const AIR_GAIN: usize = 6 * 9 + 3;
const LOW_MID_GAIN: usize = 2 * 9 + 3;

fn suite() -> MasteringSuite {
    MasteringSuite::new(48000.0)
}

fn noise_track(seconds: f32) -> AudioBuffer {
    let n = (seconds * 48000.0) as usize;
    let mut seed: u32 = 7;
    let l: Vec<f32> = (0..n)
        .map(|_| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            ((seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * 0.2
        })
        .collect();
    AudioBuffer::from_stereo(l.clone(), l, 48000)
}

fn sine_track(hz: f32) -> AudioBuffer {
    let l: Vec<f32> = (0..96000).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / 48000.0).sin() * 0.3).collect();
    AudioBuffer::from_stereo(l.clone(), l, 48000)
}

#[test]
fn test_an_answer_becomes_limited_snapped_moves() {
    let s = suite();
    let answer = r#"{"edits":[{"param":"air_db","change":1.3},{"param":"width_percent","change":99},{"param":"low_mid_db","change":-40}],"note":"Brighter."}"#;
    let plan = parse_answer(answer, &s).unwrap();
    assert_eq!(plan.note, "Brighter.");
    assert_eq!(plan.edits.len(), 3);
    let mut applied = s.clone();
    plan.apply(&mut applied);
    // +1.8 dB + 1.3 dB on the 0.5 dB grid
    assert!((applied.dynamic_eq.get_param(AIR_GAIN) - 3.0).abs() < 0.01, "{}", applied.dynamic_eq.get_param(AIR_GAIN));
    // Limited to +25% and -3 dB
    assert!((applied.imager.width - 1.40).abs() < 0.011, "{}", applied.imager.width);
    assert!((applied.dynamic_eq.get_param(LOW_MID_GAIN) + 3.5).abs() < 0.01);
    assert_eq!(plan.changes.len(), 3);
    assert!(plan.to_text().contains("Stereo Imager"));
}

#[test]
fn test_values_stay_inside_the_slider_range() {
    let mut s = suite();
    s.imager.set_param(0, 1.95);
    let plan = parse_answer(r#"{"edits":[{"param":"width_percent","change":25}]}"#, &s).unwrap();
    plan.apply(&mut s);
    assert_eq!(s.imager.width, 2.0);
}

#[test]
fn test_bad_answers_change_nothing() {
    let s = suite();
    assert!(parse_answer("not json", &s).is_err());
    for answer in [
        r#"{"edits":[{"param":"delete_everything","change":3}]}"#,
        r#"{"edits":[{"param":"air_db","change":0}]}"#,
        r#"{"edits":[]}"#,
    ] {
        assert!(parse_answer(answer, &s).unwrap().edits.is_empty(), "{}", answer);
    }
    let twice = r#"{"edits":[{"param":"air_db","change":1},{"param":"air_db","change":-3}]}"#;
    assert_eq!(parse_answer(twice, &s).unwrap().edits.len(), 1);
    let many: Vec<String> = ["bass_db", "low_mid_db", "mid_db", "upper_mid_db", "presence_db", "air_db"]
        .iter()
        .map(|p| format!(r#"{{"param":"{}","change":1}}"#, p))
        .collect();
    let plan = parse_answer(&format!(r#"{{"edits":[{}]}}"#, many.join(",")), &s).unwrap();
    assert_eq!(plan.edits.len(), MAX_EDITS);
}

#[test]
fn test_switched_off_modules_are_not_offered() {
    let mut s = suite();
    assert!(system_prompt(&s).contains("saturation_percent"));
    assert!(!system_prompt(&s).contains("compression_threshold_db"), "the Vintage Compressor is off by default");
    s.module_mut(ModuleKind::Exciter).set_enabled(false);
    s.dynamic_eq.bands[6].enabled = false;
    assert!(!system_prompt(&s).contains("saturation_percent"));
    assert!(!grammar(&s).contains("saturation_percent"));
    assert!(!grammar(&s).contains("air_db"));
    assert!(parse_answer(r#"{"edits":[{"param":"saturation_percent","change":5}]}"#, &s).unwrap().edits.is_empty());
}

#[test]
fn test_the_whole_path_with_a_stand_in_model() {
    let s = suite();
    let track = noise_track(4.0);
    let plan = plan_request(&track, &s, "  less harsh  ", -14.0, |chat, grammar| {
        let (first, last) = (&chat[0], &chat[chat.len() - 1]);
        assert!(first.0 == "system" && first.1.contains("presence_db") && first.1.contains("Answer with JSON only"));
        assert!(last.0 == "user" && last.1.contains("LUFS") && last.1.contains("spectral centroid") && last.1.ends_with("Request: less harsh"));
        assert!(chat.iter().filter(|m| m.0 == "assistant").all(|m| parse_answer(&m.1, &s).is_ok_and(|p| !p.edits.is_empty())));
        assert!(grammar.starts_with("root ::="));
        Ok(r#"{"edits":[{"param":"presence_db","change":-2}],"note":"Softer."}"#.to_string())
    })
    .unwrap();
    assert_eq!(plan.edits.len(), 1);
    assert!(plan_request(&track, &s, "   ", -14.0, |_, _| panic!("no request")).is_err());
    assert_eq!(plan_request(&track, &s, "x", -14.0, |_, _| Err("boom".to_string())).unwrap_err(), "boom");
}

#[test]
fn test_spectral_shape_tells_tone_from_noise() {
    let (tone, noise) = (spectral_shape(&sine_track(1000.0)), spectral_shape(&noise_track(2.0)));
    assert!((tone.centroid_hz - 1000.0).abs() < 100.0, "{:?}", tone);
    assert!(noise.centroid_hz > 8000.0, "{:?}", noise);
    assert!(noise.flatness > 0.5 && tone.flatness < 0.01, "{:?} {:?}", noise, tone);
    assert_eq!(spectral_shape(&AudioBuffer::from_stereo(vec![0.0; 100], vec![0.0; 100], 48000)).flatness, 0.0);
}

/// Downloads 1.1 GB, so it only runs when asked: `cargo test --release --test text_edit_test -- --ignored`
#[test]
#[ignore]
fn test_download_passes_its_checksum() {
    let path = ml_text::download_model(&AtomicBool::new(false), |_| {}).expect("download failed");
    assert_eq!(std::fs::metadata(path).unwrap().len(), ml_text::MODEL_SIZE);
}

#[test]
fn test_real_model_answers_requests() {
    if cfg!(debug_assertions) {
        eprintln!("too slow in a debug build: run with --release");
        return;
    }
    let Some(path) = ml_text::find_model() else {
        eprintln!("text model not downloaded, skipping");
        return;
    };
    let model = TextModel::load(&path).expect("model failed to load");
    let s = suite();
    let track = noise_track(4.0);
    let ask = |request: &str| -> TextPlan {
        let cancel = AtomicBool::new(false);
        let started = std::time::Instant::now();
        let plan = plan_request(&track, &s, request, -14.0, |chat, grammar| {
            let answer = model.generate(chat, grammar, MAX_TOKENS, &cancel)?;
            println!("{:?} -> {} ({:.1} s)", request, answer, started.elapsed().as_secs_f32());
            Ok(answer)
        })
        .expect("request failed");
        assert!(!plan.edits.is_empty(), "no moves for {:?}", request);
        plan
    };

    let dark = ask("much darker, less bright");
    let wide = ask("make it wider");
    for request in ["warmer, less harsh, wider", "brighter", "less muddy", "more punch in the low end", "narrower and softer", "more aggressive"] {
        ask(request);
    }
    let air = dark.edits.iter().find(|e| e.kind == ModuleKind::Equalizer && e.index == AIR_GAIN);
    assert!(air.is_some_and(|e| e.value < s.dynamic_eq.get_param(AIR_GAIN)), "{:?}", dark);
    assert!(wide.edits.iter().any(|e| e.kind == ModuleKind::Imager && e.value > s.imager.width), "{:?}", wide);

    // Greedy sampling repeats
    assert_eq!(ask("make it wider").edits, wide.edits);
}
