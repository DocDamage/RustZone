//! The space Match reference (AI) searches

use mastering_studio::dsp::{MasteringSuite, Module, ModuleKind, ParamInfo, SearchSpace};

fn every_module_on() -> MasteringSuite {
    let mut suite = MasteringSuite::new(48000.0);
    for &k in ModuleKind::ALL {
        suite.module_mut(k).set_enabled(true);
    }
    suite
}

/// Largest change one wheel notch makes at `value`
fn notch(info: &ParamInfo, value: f32) -> f32 {
    if info.log { value * info.step } else { info.step }
}

#[test]
fn test_space_starts_small_and_skips_discrete_params() {
    let space = SearchSpace::for_suite(&every_module_on(), true);
    assert!((12..=20).contains(&space.len()), "{} dimensions", space.len());
    assert!(space.activate.is_empty());
    for p in &space.params {
        assert!(p.info.min < p.info.max, "{}", p.label());
    }
}

#[test]
fn test_round_trip_lands_within_one_slider_step() {
    let suite = every_module_on();
    let space = SearchSpace::for_suite(&suite, true);
    let x = space.encode(&suite);
    assert!(x.iter().all(|v| (0.0..=1.0).contains(v)));
    let mut decoded = suite.clone();
    space.decode(&x, &mut decoded);
    for p in &space.params {
        let (a, b) = (suite.module(p.kind).get_param(p.index), decoded.module(p.kind).get_param(p.index));
        assert!((a - b).abs() <= notch(&p.info, a) + 1e-3, "{}: {} became {}", p.label(), a, b);
    }
    // Decoding the encoding of a decoded suite changes nothing
    let again = space.encode(&decoded);
    let mut twice = decoded.clone();
    space.decode(&again, &mut twice);
    for p in &space.params {
        let (a, b) = (decoded.module(p.kind).get_param(p.index), twice.module(p.kind).get_param(p.index));
        assert!((a - b).abs() <= notch(&p.info, a) + 1e-3, "{}: {} became {}", p.label(), a, b);
    }
}

#[test]
fn test_decoded_values_stay_in_range_and_log_params_are_log_mapped() {
    let mut suite = every_module_on();
    let space = SearchSpace::for_suite(&suite, true);
    for x in [0.0, 0.25, 0.5, 1.0, -1.0, 2.0] {
        space.decode(&vec![x; space.len()], &mut suite);
        for p in &space.params {
            let v = suite.module(p.kind).get_param(p.index);
            assert!(v >= p.info.min - 1e-3 && v <= p.info.max + 1e-3, "{} = {} at x = {}", p.label(), v, x);
        }
    }
    let i = space.params.iter().position(|p| p.info.log).expect("a log-mapped parameter");
    let p = &space.params[i];
    let mut x = space.encode(&suite);
    x[i] = 0.5;
    space.decode(&x, &mut suite);
    let middle = suite.module(p.kind).get_param(p.index);
    let geometric = (p.info.min * p.info.max).sqrt();
    assert!((middle - geometric).abs() <= notch(&p.info, geometric), "{} vs {}", middle, geometric);
}

#[test]
fn test_settings_outside_the_search_range_stay_reachable() {
    let mut suite = every_module_on();
    suite.maximizer.set_param(0, 20.0);
    let space = SearchSpace::for_suite(&suite, true);
    let x = space.encode(&suite);
    let mut decoded = suite.clone();
    space.decode(&x, &mut decoded);
    assert!((decoded.maximizer.get_param(0) - 20.0).abs() <= 0.5);
}

#[test]
fn test_only_enabled_modules_are_searched() {
    let mut suite = every_module_on();
    suite.module_mut(ModuleKind::Exciter).set_enabled(false);
    suite.module_mut(ModuleKind::Maximizer).set_enabled(false);

    let scoped = SearchSpace::for_suite(&suite, true);
    assert!(scoped.params.iter().all(|p| suite.module(p.kind).enabled()));
    assert!(scoped.activate.is_empty());

    let wider = SearchSpace::for_suite(&suite, false);
    assert!(wider.len() > scoped.len());
    assert_eq!(wider.activate, vec![ModuleKind::Exciter, ModuleKind::Maximizer]);
    let mut decoded = suite.clone();
    wider.decode(&wider.encode(&suite), &mut decoded);
    assert!(decoded.module(ModuleKind::Exciter).enabled());
}

#[test]
fn test_eq_bands_the_user_switched_off_or_reshaped_are_left_alone() {
    let mut suite = every_module_on();
    let full = SearchSpace::for_suite(&suite, true).len();
    suite.dynamic_eq.bands[2].enabled = false;
    suite.dynamic_eq.set_param(6 * 9 + 1, 4.0); // band 7 becomes a low-pass
    assert_eq!(SearchSpace::for_suite(&suite, true).len(), full - 4);
}

#[test]
fn test_ceiling_is_held_and_changes_are_reported() {
    let suite = every_module_on();
    let space = SearchSpace::for_suite(&suite, true).with_ceiling(-2.0);
    let mut x = space.encode(&suite);
    x[0] = (x[0] + 0.3).min(1.0);
    let mut decoded = suite.clone();
    space.decode(&x, &mut decoded);
    assert_eq!(decoded.maximizer.get_param(1), -2.0);

    let changes = space.changes(&x, &suite);
    assert!(changes.iter().any(|c| c.name.contains("B2 frequency")), "{:?}", changes);
    assert!(changes.iter().any(|c| c.name.contains("Ceiling")), "{:?}", changes);
    assert!(changes.iter().all(|c| c.before != c.after), "{:?}", changes);
}
