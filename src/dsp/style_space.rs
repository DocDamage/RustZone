//! The parameters Match reference (AI) may change, as a flat vector in [0, 1] for the optimiser.
//!
//! Toggles, choices, chain order and oversampling stay as the user set them. Ranges are tighter
//! than the sliders', so a search can't wander into settings that score well but sound bad.

use super::module::{ModuleKind, ParamInfo, ParamUnit};
use super::MasteringSuite;
use crate::dsp::EqFilterType;

const EQ_PARAMS_PER_BAND: usize = 9;
const EQ_FREQUENCY: usize = 2;
const EQ_GAIN: usize = 3;
const EQ_GAIN_RANGE: (f32, f32) = (-9.0, 9.0);
/// Low shelf, two bells and high shelf: band index and frequency range
const EQ_BANDS: [(usize, (f32, f32)); 4] =
    [(1, (30.0, 300.0)), (2, (100.0, 1000.0)), (5, (1500.0, 9000.0)), (6, (4000.0, 16000.0))];

/// Module, parameter index and search range of everything outside the EQ
const SEARCHED: [(ModuleKind, usize, (f32, f32)); 7] = [
    (ModuleKind::VintageCompressor, 0, (-36.0, 0.0)),
    (ModuleKind::VintageCompressor, 1, (1.0, 8.0)),
    (ModuleKind::VintageCompressor, 6, (0.0, 1.0)),
    (ModuleKind::Imager, 0, (0.5, 1.6)),
    (ModuleKind::Exciter, 1, (0.0, 1.0)),
    (ModuleKind::Exciter, 2, (0.0, 1.0)),
    (ModuleKind::Maximizer, 0, (0.0, 12.0)),
];

const MAXIMIZER_CEILING: usize = 1;

/// Whether an EQ band is on and has a shape its gain affects
pub fn eq_band_can_shape_gain(suite: &MasteringSuite, band: usize) -> bool {
    let b = &suite.dynamic_eq.bands[band];
    b.enabled && !matches!(b.filter_type, EqFilterType::HighPass | EqFilterType::LowPass)
}

/// One searched parameter; `info` carries the search range rather than the slider's
#[derive(Clone, Debug)]
pub struct SearchParam {
    pub kind: ModuleKind,
    pub index: usize,
    pub info: ParamInfo,
}

impl SearchParam {
    fn encode(&self, value: f32) -> f32 {
        let (min, max) = (self.info.min, self.info.max);
        let x = if self.info.log {
            (value.max(min) / min).ln() / (max / min).ln()
        } else {
            (value - min) / (max - min)
        };
        x.clamp(0.0, 1.0)
    }

    fn decode(&self, x: f32) -> f32 {
        let (min, max) = (self.info.min, self.info.max);
        let x = x.clamp(0.0, 1.0);
        let value = if self.info.log { min * (max / min).powf(x) } else { min + (max - min) * x };
        self.info.snapped(value)
    }

    /// Display name, with the module's name in front
    pub fn label(&self) -> String {
        format!("{} › {}", self.kind.name(), self.info.name)
    }
}

/// A parameter a match changed, formatted for the report
#[derive(Clone, Debug, PartialEq)]
pub struct ParamChange {
    pub name: String,
    pub before: String,
    pub after: String,
}

#[derive(Clone, Debug, Default)]
pub struct SearchSpace {
    pub params: Vec<SearchParam>,
    /// Switched-off modules this space works on, which `decode` switches on
    pub activate: Vec<ModuleKind>,
    /// Maximizer ceiling held fixed through the search
    pub ceiling_db: Option<f32>,
}

impl SearchSpace {
    /// The space for a suite. With `only_enabled`, switched-off modules are left out; otherwise
    /// they're searched too and a match switches them on.
    pub fn for_suite(suite: &MasteringSuite, only_enabled: bool) -> Self {
        let mut space = Self::default();
        let eq_params = EQ_BANDS.iter().filter(|(band, _)| eq_band_can_shape_gain(suite, *band));
        let eq_params = eq_params.flat_map(|&(band, hz)| {
            let base = band * EQ_PARAMS_PER_BAND;
            [(ModuleKind::Equalizer, base + EQ_FREQUENCY, hz), (ModuleKind::Equalizer, base + EQ_GAIN, EQ_GAIN_RANGE)]
        });
        for (kind, index, range) in eq_params.chain(SEARCHED) {
            let module = suite.module(kind);
            if !module.enabled() {
                if only_enabled {
                    continue;
                }
                if !space.activate.contains(&kind) {
                    space.activate.push(kind);
                }
            }
            let Some(info) = module.params().into_iter().nth(index) else { continue };
            if matches!(info.unit, ParamUnit::Toggle | ParamUnit::Choice(_)) {
                continue;
            }
            // A setting already outside the range stays reachable
            let value = module.get_param(index);
            let info = ParamInfo { min: range.0.min(value).max(info.min), max: range.1.max(value).min(info.max), ..info };
            space.params.push(SearchParam { kind, index, info });
        }
        space
    }

    /// Hold the maximizer's ceiling at `ceiling_db`, if the maximizer is part of the space
    pub fn with_ceiling(mut self, ceiling_db: f32) -> Self {
        if self.params.iter().any(|p| p.kind == ModuleKind::Maximizer) {
            self.ceiling_db = Some(ceiling_db);
        }
        self
    }

    pub fn len(&self) -> usize {
        self.params.len()
    }

    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }

    /// The suite's settings as a point in the space
    pub fn encode(&self, suite: &MasteringSuite) -> Vec<f32> {
        self.params.iter().map(|p| p.encode(suite.module(p.kind).get_param(p.index))).collect()
    }

    /// Set the suite to a point in the space, on the sliders' grid
    pub fn decode(&self, x: &[f32], suite: &mut MasteringSuite) {
        for &kind in &self.activate {
            suite.module_mut(kind).set_enabled(true);
        }
        for (p, &v) in self.params.iter().zip(x) {
            suite.module_mut(p.kind).set_param(p.index, p.decode(v));
        }
        if let Some(ceiling) = self.ceiling_db {
            suite.module_mut(ModuleKind::Maximizer).set_param(MAXIMIZER_CEILING, ceiling);
        }
    }

    /// What `decode(x, ...)` changes in `before`
    pub fn changes(&self, x: &[f32], before: &MasteringSuite) -> Vec<ParamChange> {
        let mut after = before.clone();
        self.decode(x, &mut after);
        let mut changes: Vec<ParamChange> = self
            .activate
            .iter()
            .map(|k| ParamChange { name: k.name().to_string(), before: "OFF".to_string(), after: "ON".to_string() })
            .collect();
        let mut compare = |name: String, info: &ParamInfo, old: f32, new: f32| {
            if (new - old).abs() > 1e-4 {
                changes.push(ParamChange { name, before: info.format(old), after: info.format(new) });
            }
        };
        for p in &self.params {
            let (old, new) = (before.module(p.kind).get_param(p.index), after.module(p.kind).get_param(p.index));
            compare(p.label(), &p.info, old, new);
        }
        if self.ceiling_db.is_some() {
            let kind = ModuleKind::Maximizer;
            if let Some(info) = before.module(kind).params().get(MAXIMIZER_CEILING) {
                let label = format!("{} › {}", kind.name(), info.name);
                compare(label, info, before.module(kind).get_param(MAXIMIZER_CEILING), after.module(kind).get_param(MAXIMIZER_CEILING));
            }
        }
        changes
    }
}
