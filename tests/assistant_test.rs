//! Master Assistant: genre targets and reference-aware chain suggestions

use mastering_studio::audio::AudioBuffer;
use mastering_studio::dsp::{AssistantAction, AssistantOptions, BiquadFilter, FilterType, MasterAssistant, MasteringSuite, Module, ModuleKind, GENRES};

const SR: u32 = 44100;

/// Pink noise (Paul Kellet's filter), independent per channel when `decorrelated`
fn pink(seconds: f32, seed: u64, decorrelated: bool) -> (Vec<f32>, Vec<f32>) {
    let n = (SR as f32 * seconds) as usize;
    let gen = |seed: u64| {
        let mut s = seed;
        let mut b = [0.0f32; 7];
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let w = (s >> 33) as f32 / (1u64 << 31) as f32 * 2.0 - 1.0;
                b[0] = 0.99886 * b[0] + w * 0.0555179;
                b[1] = 0.99332 * b[1] + w * 0.0750759;
                b[2] = 0.96900 * b[2] + w * 0.153_852;
                b[3] = 0.86650 * b[3] + w * 0.3104856;
                b[4] = 0.55000 * b[4] + w * 0.5329522;
                b[5] = -0.7616 * b[5] - w * 0.0168980;
                let out = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
                b[6] = w * 0.115926;
                out * 0.05
            })
            .collect::<Vec<f32>>()
    };
    let l = gen(seed);
    let r = if decorrelated { gen(seed ^ 0xABCDEF) } else { l.clone() };
    (l, r)
}

fn filtered(x: &(Vec<f32>, Vec<f32>), kind: FilterType, hz: f32) -> (Vec<f32>, Vec<f32>) {
    let mut f = BiquadFilter::new(kind, hz, 0.707, 0.0, SR as f32);
    x.0.iter().zip(&x.1).map(|(&l, &r)| f.process_sample(l, r)).unzip()
}

fn buffer(x: (Vec<f32>, Vec<f32>)) -> AudioBuffer {
    AudioBuffer::from_stereo(x.0, x.1, SR)
}

fn opts(genre: usize) -> AssistantOptions<'static> {
    AssistantOptions { target_lufs: -14.0, ceiling_dbtp: -1.0, genre, reference: None }
}

fn eq_gain(actions: &[AssistantAction], band: usize) -> f32 {
    actions
        .iter()
        .find_map(|a| match a {
            AssistantAction::Eq { band: b, gain_db, .. } if *b == band => Some(*gain_db),
            _ => None,
        })
        .unwrap_or(0.0)
}

fn enables(actions: &[AssistantAction], kind: ModuleKind) -> Option<bool> {
    actions.iter().rev().find_map(|a| match a {
        AssistantAction::Enable(k, on) if *k == kind => Some(*on),
        _ => None,
    })
}

#[test]
fn test_dull_and_bright_mixes_get_opposite_eq() {
    let noise = pink(8.0, 1, true);
    let dull = buffer(filtered(&noise, FilterType::LowPass, 3000.0));
    let bright = buffer(filtered(&noise, FilterType::HighPass, 400.0));

    let r = MasterAssistant::analyze_with(&dull, &opts(1)).unwrap();
    println!("dull:\n{}", r.to_text());
    assert!(eq_gain(&r.actions, 4) > 1.0, "a dull mix should get air");
    assert!(eq_gain(&r.actions, 3) > 0.5, "and presence");
    assert_eq!(enables(&r.actions, ModuleKind::SpectralShaper), Some(false));

    let r = MasterAssistant::analyze_with(&bright, &opts(1)).unwrap();
    println!("bright:\n{}", r.to_text());
    assert!(eq_gain(&r.actions, 0) > 1.0, "a thin mix should get low end");
    assert_eq!(enables(&r.actions, ModuleKind::SpectralShaper), Some(true), "a bright mix should be tamed");
    assert_eq!(enables(&r.actions, ModuleKind::BassEnhancer), Some(true), "and its missing sub supported");
}

#[test]
fn test_genre_changes_the_targets() {
    let mix = buffer(pink(8.0, 2, true));
    let hiphop = GENRES.iter().position(|g| g.name.starts_with("Hip-hop")).unwrap();
    let classical = GENRES.iter().position(|g| g.name.starts_with("Classical")).unwrap();
    let a = MasterAssistant::analyze_with(&mix, &opts(hiphop)).unwrap();
    let b = MasterAssistant::analyze_with(&mix, &opts(classical)).unwrap();
    assert_eq!(a.target_name, GENRES[hiphop].name);
    // Hip-hop wants far more low end than classical music
    assert!(eq_gain(&a.actions, 0) > eq_gain(&b.actions, 0) + 1.0);
    // Classical masters keep their dynamics, low end and harmonics alone
    assert!(b.target_plr_db > a.target_plr_db + 5.0);
    assert_eq!(enables(&b.actions, ModuleKind::Exciter), Some(false));
    assert!(b.recommended_mono_maker_hz < a.recommended_mono_maker_hz);
}

#[test]
fn test_dynamics_suggestions_follow_peak_to_loudness() {
    // Sparse loud hits over quiet noise: very dynamic
    let (mut l, mut r) = pink(8.0, 3, true);
    for v in l.iter_mut().chain(r.iter_mut()) {
        *v *= 0.1;
    }
    for i in (0..l.len()).step_by(SR as usize / 2) {
        for j in 0..200.min(l.len() - i) {
            let hit = 0.9 * (-(j as f32) / 40.0).exp();
            l[i + j] += hit;
            r[i + j] += hit;
        }
    }
    let r_dyn = MasterAssistant::analyze_with(&buffer((l, r)), &opts(1)).unwrap();
    assert_eq!(enables(&r_dyn.actions, ModuleKind::VintageCompressor), Some(true), "PLR {:.1}", r_dyn.plr_db);

    // Hard-clipped, dense noise: already squashed
    let (l, r) = pink(8.0, 4, true);
    let squash = |v: Vec<f32>| v.into_iter().map(|x| (x * 12.0).clamp(-0.5, 0.5)).collect::<Vec<f32>>();
    let r_dense = MasterAssistant::analyze_with(&buffer((squash(l), squash(r))), &opts(1)).unwrap();
    assert_eq!(enables(&r_dense.actions, ModuleKind::VintageCompressor), Some(false));
    assert_eq!(enables(&r_dense.actions, ModuleKind::TransientRestore), Some(true), "PLR {:.1}", r_dense.plr_db);
}

#[test]
fn test_reference_track_becomes_the_target() {
    let track = buffer(filtered(&pink(8.0, 5, false), FilterType::LowPass, 4000.0)); // dark and mono
    let reference = buffer(pink(8.0, 6, true)); // full range and wide
    let o = AssistantOptions { reference: Some(&reference), ..opts(0) };
    let r = MasterAssistant::analyze_with(&track, &o).unwrap();
    println!("{}", r.to_text());
    assert_eq!(r.target_name, "Reference track");
    assert!(r.reference_lufs.is_some());
    assert!(r.actions.iter().any(|a| matches!(a, AssistantAction::MatchEq { .. })), "tone comes from Match EQ");
    // Broad EQ moves would fight Match EQ
    assert_eq!(eq_gain(&r.actions, 4), 0.0);
    assert!(r.recommended_stereo_width > 1.2, "a mono track should be widened toward a wide reference");

    let mut suite = MasteringSuite::new(SR as f32);
    MasterAssistant::apply_to_suite(&r, &mut suite);
    assert!(suite.match_eq.enabled() && suite.match_eq.is_learned());
    assert!((suite.match_eq.get_param(0) - 0.6).abs() < 1e-6);
    assert_eq!(suite.imager.width, r.recommended_stereo_width);
}

#[test]
fn test_apply_switches_the_suggested_chain() {
    let bright = buffer(filtered(&pink(8.0, 7, true), FilterType::HighPass, 400.0));
    let r = MasterAssistant::analyze_with(&bright, &opts(1)).unwrap();
    let mut suite = MasteringSuite::new(SR as f32);
    suite.vintage_comp.set_enabled(true);
    MasterAssistant::apply_to_suite(&r, &mut suite);
    for (kind, on) in r.actions.iter().filter_map(|a| match a {
        AssistantAction::Enable(k, on) => Some((*k, *on)),
        _ => None,
    }) {
        assert_eq!(suite.module(kind).enabled(), on, "{:?}", kind);
    }
    // Only bands with a suggested move are on
    for (i, band) in suite.dynamic_eq.bands.iter().enumerate() {
        assert_eq!(band.enabled, eq_gain(&r.actions, i) != 0.0, "EQ band {}", i);
    }
    assert!(suite.maximizer.threshold_db < 0.0);
}
