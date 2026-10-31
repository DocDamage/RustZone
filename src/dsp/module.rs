//! Common interface for every processor in the mastering chain.
//!
//! Each module describes its parameters with [`ParamInfo`], so the editor panel, presets and
//! chain ordering work the same way for all of them without per-module UI code.

/// How a parameter's value is shown and stepped
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamUnit {
    Db,
    Hz,
    /// Stored as 0..1 (or -1..1), shown as a percentage
    Percent,
    Ratio,
    Ms,
    Plain,
    Toggle,
    Choice(&'static [&'static str]),
}

#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub name: String,
    pub unit: ParamUnit,
    pub min: f32,
    pub max: f32,
    /// Step per wheel notch (multiplicative ratio - 1 when `log` is true)
    pub step: f32,
    pub log: bool,
}

impl ParamInfo {
    pub fn new(name: impl Into<String>, unit: ParamUnit, min: f32, max: f32, step: f32) -> Self {
        Self { name: name.into(), unit, min, max, step, log: false }
    }

    pub fn hz(name: impl Into<String>, min: f32, max: f32) -> Self {
        Self { name: name.into(), unit: ParamUnit::Hz, min, max, step: 0.06, log: true }
    }

    pub fn toggle(name: impl Into<String>) -> Self {
        Self::new(name, ParamUnit::Toggle, 0.0, 1.0, 1.0)
    }

    pub fn choice(name: impl Into<String>, options: &'static [&'static str]) -> Self {
        Self::new(name, ParamUnit::Choice(options), 0.0, (options.len() - 1) as f32, 1.0)
    }

    pub fn percent(name: impl Into<String>, min: f32, max: f32) -> Self {
        Self::new(name, ParamUnit::Percent, min, max, 0.01)
    }

    pub fn db(name: impl Into<String>, min: f32, max: f32) -> Self {
        Self::new(name, ParamUnit::Db, min, max, 0.5)
    }

    /// Value after `steps` notches (negative = down); toggles flip and choices cycle
    pub fn stepped(&self, value: f32, steps: i32, fine: bool) -> f32 {
        match self.unit {
            ParamUnit::Toggle => {
                if steps == 0 { value } else if value > 0.5 { 0.0 } else { 1.0 }
            }
            ParamUnit::Choice(opts) => {
                let n = opts.len() as i32;
                ((value.round() as i32 + steps).rem_euclid(n)) as f32
            }
            _ => {
                let k = if fine { 0.1 } else { 1.0 };
                let v = if self.log {
                    value.max(1e-6) * (1.0 + self.step * k).powi(steps)
                } else {
                    value + self.step * k * steps as f32
                };
                v.clamp(self.min, self.max)
            }
        }
    }

    pub fn format(&self, value: f32) -> String {
        match self.unit {
            ParamUnit::Db => format!("{:+.1} dB", value),
            ParamUnit::Hz => {
                if value >= 1000.0 { format!("{:.2} kHz", value / 1000.0) } else { format!("{:.0} Hz", value) }
            }
            ParamUnit::Percent => format!("{:.0}%", value * 100.0),
            ParamUnit::Ratio => format!("{:.1}:1", value),
            ParamUnit::Ms => format!("{:.0} ms", value),
            ParamUnit::Plain => format!("{:.2}", value),
            ParamUnit::Toggle => if value > 0.5 { "ON".to_string() } else { "OFF".to_string() },
            ParamUnit::Choice(opts) => opts.get(value.round().max(0.0) as usize).unwrap_or(&"?").to_string(),
        }
    }
}

/// A processor in the mastering chain
pub trait Module {
    fn enabled(&self) -> bool;
    fn set_enabled(&mut self, enabled: bool);
    fn params(&self) -> Vec<ParamInfo>;
    fn get_param(&self, index: usize) -> f32;
    fn set_param(&mut self, index: usize, value: f32);
    fn set_sample_rate(&mut self, sample_rate: f32);
    fn reset(&mut self);
    fn process(&mut self, left: &mut [f32], right: &mut [f32]);
    /// Processing delay in samples while enabled
    fn latency(&self) -> usize {
        0
    }
    /// One-line live readout (gain reduction, learn status...) for the editor
    fn status(&self) -> Option<String> {
        None
    }
    /// Live values (gain reduction...) published from the audio thread's copy of the module
    fn meters(&self) -> [f32; 4] {
        [0.0; 4]
    }
    /// Receive published live values on the UI's copy, so `status` reads them
    fn set_meters(&mut self, _meters: [f32; 4]) {}
    /// Default oversampling factor for modules whose nonlinearities alias (0 = not offered)
    fn oversampling_default(&self) -> usize {
        0
    }
    /// Hard output ceiling (linear) that must hold at the suite rate. When the module is
    /// oversampled, the downsampling filter can overshoot it, so the suite re-applies it.
    fn output_ceiling(&self) -> Option<f32> {
        None
    }
}

macro_rules! module_kinds {
    ($( $variant:ident => $key:literal, $name:literal; )*) => {
        /// Every module the suite offers, in registry order
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum ModuleKind { $( $variant, )* }

        impl ModuleKind {
            pub const ALL: &'static [ModuleKind] = &[ $( ModuleKind::$variant, )* ];
            pub const COUNT: usize = [ $( stringify!($variant), )* ].len();

            /// Position in `ALL`
            pub fn index(self) -> usize {
                self as usize
            }

            /// Stable identifier used in presets
            pub fn key(self) -> &'static str {
                match self { $( ModuleKind::$variant => $key, )* }
            }

            pub fn name(self) -> &'static str {
                match self { $( ModuleKind::$variant => $name, )* }
            }

            pub fn from_key(key: &str) -> Option<ModuleKind> {
                match key { $( $key => Some(ModuleKind::$variant), )* _ => None }
            }
        }
    };
}

module_kinds! {
    Equalizer => "equalizer", "Equalizer (8-band dynamic)";
    PassiveEq => "passive_eq", "Vintage Passive EQ";
    MatchEq => "match_eq", "Match EQ";
    StemEq => "stem_eq", "Stem EQ";
    StemRebalance => "stem_rebalance", "Stem Rebalance";
    LowEnd => "low_end", "Low-End Shaper";
    BassEnhancer => "bass", "Bass Enhancer";
    Dynamics => "dynamics", "Multiband Dynamics";
    VintageCompressor => "vintage_comp", "Vintage Compressor";
    Transient => "transient", "Multiband Transient Shaper";
    TransientRestore => "transient_restore", "Transient Restore";
    SpectralShaper => "spectral_shaper", "Spectral Shaper";
    Stabilizer => "stabilizer", "Tonal Stabilizer";
    Clarity => "clarity", "Clarity";
    Imager => "imager", "Stereo Imager";
    Exciter => "exciter", "Harmonic Exciter";
    Tape => "tape", "Tape Saturation";
    OptoLimiter => "opto_limiter", "Vintage Opto Limiter";
    Maximizer => "maximizer", "True-Peak Maximizer";
}

/// Envelope follower coefficient for a time constant in milliseconds
#[inline]
pub fn time_coeff(ms: f32, sample_rate: f32) -> f32 {
    (-1.0 / (ms.max(0.01) * 0.001 * sample_rate.max(1.0))).exp()
}

#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

#[inline]
pub fn gain_to_db(g: f32) -> f32 {
    20.0 * g.max(1e-9).log10()
}

/// Attack/release envelope follower on a rectified signal
#[derive(Clone, Copy, Debug, Default)]
pub struct Envelope {
    pub value: f32,
}

impl Envelope {
    #[inline]
    pub fn follow(&mut self, input: f32, attack: f32, release: f32) -> f32 {
        let c = if input > self.value { attack } else { release };
        self.value = input + c * (self.value - input);
        self.value
    }
}
