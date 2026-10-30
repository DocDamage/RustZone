//! Engine tests that run against the real audio thread (need an output device)

use mastering_studio::audio::{AudioBuffer, AudioEngine, ProcessingMode};
use mastering_studio::dsp::{calculate_buffer_integrated_lufs, MasteringSuite, Module, ModuleKind};
use std::time::Duration;

fn noise(n: usize, seed: u64, amp: f32) -> Vec<f32> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((s >> 33) as f32 / (1u64 << 31) as f32 * 2.0 - 1.0) * amp
        })
        .collect()
}

fn engine() -> Option<AudioEngine> {
    match AudioEngine::new() {
        Ok(e) => Some(e),
        Err(e) => {
            eprintln!("No audio device ({}), skipping", e);
            None
        }
    }
}

#[test]
fn test_edits_reach_the_audio_thread_without_blocking() {
    let Some(engine) = engine() else { return };
    for i in 0..200 {
        let drive = (i % 20) as f32;
        engine.edit_dsp_suite(move |s| s.maximizer.threshold_db = -drive);
    }
    assert!(engine.flush_commands(), "audio thread did not apply queued edits");
    engine.with_dsp_suite(|s| assert_eq!(s.maximizer.threshold_db, -19.0));

    // Detaching a plugin when none is loaded must not hang
    let r = engine.with_detached_vst3_plugin(|p| p.is_none());
    assert!(r);
    engine.collect_garbage();
}

#[test]
fn test_loudness_matched_ab_turns_processing_down() {
    let Some(engine) = engine() else { return };
    let sr = engine.device_sample_rate();
    let n = sr as usize * 10;
    let l = noise(n, 5, 0.05);
    engine.set_audio_buffer(AudioBuffer::from_stereo(l.clone(), l, sr));
    engine.set_processing_mode(ProcessingMode::DspSuite);
    // +12 dB of maximizer drive makes the processed signal much louder than the input
    engine.edit_dsp_suite(|s| {
        for &k in ModuleKind::ALL {
            s.module_mut(k).set_enabled(false);
        }
        s.maximizer.set_enabled(true);
        s.maximizer.threshold_db = -12.0;
    });
    engine.set_ab_gain_match(true);
    engine.play();
    std::thread::sleep(Duration::from_millis(2500));
    let meters = engine.get_meter_levels();
    engine.stop();
    assert!(meters.ab_match_db < -6.0, "A/B match only applied {:.1} dB", meters.ab_match_db);
}

#[test]
fn test_auto_gain_cancels_a_modules_level_change() {
    let sr = 48000.0;
    let n = sr as usize * 8;
    let input = noise(n, 21, 0.1);

    let render = |auto: bool| -> f32 {
        let mut suite = MasteringSuite::new(sr);
        for &k in ModuleKind::ALL {
            suite.module_mut(k).set_enabled(false);
        }
        suite.passive_eq.set_enabled(true);
        suite.passive_eq.set_param(1, 8.0); // low boost
        suite.passive_eq.set_param(4, 8.0); // high boost
        suite.set_auto_gain(ModuleKind::PassiveEq, auto);
        let (mut l, mut r) = (input.clone(), input.clone());
        for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            suite.process_stereo(cl, cr);
        }
        let tail = n / 2;
        calculate_buffer_integrated_lufs(&AudioBuffer::from_stereo(l[tail..].to_vec(), r[tail..].to_vec(), sr as u32))
    };
    let dry = calculate_buffer_integrated_lufs(&AudioBuffer::from_stereo(input[n / 2..].to_vec(), input[n / 2..].to_vec(), sr as u32));
    let boosted = render(false);
    let matched = render(true);
    assert!(boosted - dry > 2.0, "test EQ should be louder: {:.1} vs {:.1}", boosted, dry);
    assert!((matched - dry).abs() < 1.0, "auto gain left {:.1} dB difference", matched - dry);
}
