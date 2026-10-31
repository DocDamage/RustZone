pub mod k_weighting;
pub mod lufs;
pub mod fft;
pub mod dynamic_eq;
pub mod multiband;
pub mod maximizer;
pub mod clarity;
pub mod dither;
pub mod preset_parser;
pub mod assistant;
pub mod module;
pub mod stft;
pub mod oversampling;
pub mod spectral;
pub mod transient;
pub mod vintage;
pub mod style_space;
pub mod style_match;
mod core_modules;

pub use lufs::*;
pub use k_weighting::LoudnessFollower;
pub use fft::*;
pub use dynamic_eq::*;
pub use multiband::*;
pub use maximizer::*;
pub use clarity::*;
pub use dither::*;
pub use preset_parser::*;
pub use assistant::*;
pub use module::{Module, ModuleKind, ParamInfo, ParamUnit};
pub use spectral::*;
pub use transient::*;
pub use vintage::*;
pub use style_space::{SearchParam, SearchSpace};
pub use style_match::{match_to_reference, MatchOptions, MatchProgress, MatchResult};

use std::f32::consts::PI;

/// General Biquad filter types based on RBJ Audio EQ Cookbook
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilterType {
    LowShelf,
    Peaking,
    HighShelf,
    HighPass,
    LowPass,
}

#[derive(Clone, Debug)]
pub struct BiquadFilter {
    filter_type: FilterType,
    freq: f32,
    q: f32,
    gain_db: f32,
    sample_rate: f32,
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1_l: f32,
    z2_l: f32,
    z1_r: f32,
    z2_r: f32,
}

impl BiquadFilter {
    pub fn new(filter_type: FilterType, freq: f32, q: f32, gain_db: f32, sample_rate: f32) -> Self {
        let mut filter = Self {
            filter_type,
            freq,
            q,
            gain_db,
            sample_rate,
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1_l: 0.0,
            z2_l: 0.0,
            z1_r: 0.0,
            z2_r: 0.0,
        };
        filter.update_coefficients();
        filter
    }

    pub fn set_params(&mut self, freq: f32, q: f32, gain_db: f32) {
        self.freq = freq.clamp(20.0, self.sample_rate * 0.49);
        self.q = q.max(0.01);
        self.gain_db = gain_db.clamp(-24.0, 24.0);
        self.update_coefficients();
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.update_coefficients();
    }

    pub fn reset_state(&mut self) {
        self.z1_l = 0.0;
        self.z2_l = 0.0;
        self.z1_r = 0.0;
        self.z2_r = 0.0;
    }

    fn update_coefficients(&mut self) {
        let sr = self.sample_rate.max(1000.0);
        let f0 = self.freq.clamp(20.0, sr * 0.49);
        let w0 = 2.0 * PI * f0 / sr;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * self.q.max(0.1));
        let a = 10.0f32.powf(self.gain_db / 40.0);

        let (b0, b1, b2, a0, a1, a2) = match self.filter_type {
            FilterType::Peaking => {
                let b0 = 1.0 + alpha * a;
                let b1 = -2.0 * cos_w0;
                let b2 = 1.0 - alpha * a;
                let a0 = 1.0 + alpha / a;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha / a;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterType::LowShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
                let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
                let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
                let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
                let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
                let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterType::HighShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
                let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
                let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
                let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
                let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
                let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterType::HighPass => {
                let b0 = (1.0 + cos_w0) * 0.5;
                let b1 = -(1.0 + cos_w0);
                let b2 = (1.0 + cos_w0) * 0.5;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            FilterType::LowPass => {
                let b0 = (1.0 - cos_w0) * 0.5;
                let b1 = 1.0 - cos_w0;
                let b2 = (1.0 - cos_w0) * 0.5;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
        };

        let inv_a0 = 1.0 / a0;
        self.b0 = b0 * inv_a0;
        self.b1 = b1 * inv_a0;
        self.b2 = b2 * inv_a0;
        self.a1 = a1 * inv_a0;
        self.a2 = a2 * inv_a0;
    }

    #[inline]
    pub fn process_sample(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        let out_l = self.b0 * in_l + self.z1_l;
        self.z1_l = self.b1 * in_l - self.a1 * out_l + self.z2_l;
        self.z2_l = self.b2 * in_l - self.a2 * out_l;

        let out_r = self.b0 * in_r + self.z1_r;
        self.z1_r = self.b1 * in_r - self.a1 * out_r + self.z2_r;
        self.z2_r = self.b2 * in_r - self.a2 * out_r;

        (out_l, out_r)
    }
}

/// Multiband Mid/Side Stereo Imager with bass mono-maker and Stereoize
#[derive(Clone, Debug)]
pub struct MasteringImager {
    pub enabled: bool,
    pub width: f32, // 0.0 = mono, 1.0 = normal, 2.0 = extra wide (applies to every band)
    pub mono_bass_freq: f32,
    /// Per-band width multipliers below 200 Hz, 200 Hz-2 kHz, 2-8 kHz, above 8 kHz
    pub band_width: [f32; 4],
    /// Adds decorrelated width to narrow material (0..1)
    pub stereoize: f32,
    mono_filter: BiquadFilter,
    splits: multiband::Crossover4,
    stereoize_hp: BiquadFilter,
    delay: Vec<f32>,
    delay_pos: usize,
    sample_rate: f32,
}

const IMAGER_CROSSOVERS: [f32; 3] = [200.0, 2000.0, 8000.0];

impl MasteringImager {
    pub fn new(sample_rate: f32) -> Self {
        let mut im = Self {
            enabled: true,
            width: 1.15,
            mono_bass_freq: 120.0,
            band_width: [1.0; 4],
            stereoize: 0.0,
            mono_filter: BiquadFilter::new(FilterType::HighPass, 120.0, 0.707, 0.0, sample_rate),
            splits: multiband::Crossover4::new(IMAGER_CROSSOVERS, sample_rate),
            stereoize_hp: BiquadFilter::new(FilterType::HighPass, 300.0, 0.707, 0.0, sample_rate),
            delay: Vec::new(),
            delay_pos: 0,
            sample_rate,
        };
        im.set_sample_rate(sample_rate);
        im
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.mono_filter.set_sample_rate(sample_rate);
        self.stereoize_hp.set_sample_rate(sample_rate);
        self.splits = multiband::Crossover4::new(IMAGER_CROSSOVERS, sample_rate);
        // ~12 ms decorrelation delay for Stereoize
        self.delay = vec![0.0; ((sample_rate * 0.012) as usize).max(1)];
        self.delay_pos = 0;
    }

    pub fn reset_state(&mut self) {
        self.mono_filter.reset_state();
        self.stereoize_hp.reset_state();
        self.splits.reset();
        self.delay.fill(0.0);
    }

    pub fn set_bass_mono_freq(&mut self, freq: f32) {
        self.mono_bass_freq = freq;
        self.mono_filter.set_params(freq, 0.707, 0.0);
    }

    #[inline]
    pub fn set_mono_cutoff(&mut self, freq: f32) {
        self.set_bass_mono_freq(freq);
    }

    pub fn process_block(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.enabled {
            return;
        }
        let multiband = self.band_width.iter().any(|w| (w - 1.0).abs() > 1e-4);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mid = (*l + *r) * 0.5;
            let raw_side = (*l - *r) * 0.5;

            // Force sub-bass to 100% mono
            let (filtered_side, _) = self.mono_filter.process_sample(raw_side, raw_side);
            let mut out_side = if multiband {
                let [b0, b1, b2, b3] = self.splits.split(filtered_side, 0.0);
                b0.0 * self.band_width[0] + b1.0 * self.band_width[1] + b2.0 * self.band_width[2] + b3.0 * self.band_width[3]
            } else {
                filtered_side
            } * self.width;

            if self.stereoize > 0.0 {
                let delayed = self.delay[self.delay_pos];
                self.delay[self.delay_pos] = mid;
                self.delay_pos = (self.delay_pos + 1) % self.delay.len();
                let (decor, _) = self.stereoize_hp.process_sample(delayed, delayed);
                out_side += decor * self.stereoize * 0.5;
            }

            *l = mid + out_side;
            *r = mid - out_side;
        }
    }
}

pub const EXCITER_MODES: &[&str] = &["Warm", "Tube", "Tape", "Crisp"];

/// Harmonic Exciter with selectable saturation character
#[derive(Clone, Debug)]
pub struct MasteringExciter {
    pub enabled: bool,
    pub drive: f32,
    pub mix: f32,
    pub warmth: f32,
    pub mode: usize,
    pub crossover_hz: f32,
    high_crossover: BiquadFilter,
}

impl MasteringExciter {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            enabled: true,
            drive: 0.15,
            mix: 0.30,
            warmth: 0.20,
            mode: 0,
            crossover_hz: 2500.0,
            high_crossover: BiquadFilter::new(FilterType::HighPass, 2500.0, 0.707, 0.0, sample_rate),
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.high_crossover.set_sample_rate(sample_rate);
    }

    pub fn reset_state(&mut self) {
        self.high_crossover.reset_state();
    }

    pub fn set_crossover(&mut self, hz: f32) {
        self.crossover_hz = hz.clamp(500.0, 12000.0);
        self.high_crossover.set_params(self.crossover_hz, 0.707, 0.0);
    }

    #[inline]
    fn saturate(&self, x: f32) -> f32 {
        let gain = 1.0 + self.drive * 4.0;
        let even = (x * x * self.warmth * 0.5).copysign(x);
        match self.mode {
            // Tube: asymmetric transfer adds even harmonics
            1 => ((x * gain + 0.2 * self.warmth).tanh() - (0.2 * self.warmth).tanh()) / gain + even * 0.5,
            // Tape: soft cubic knee, mostly odd harmonics
            2 => {
                let y = (x * gain).clamp(-1.5, 1.5);
                (y - y * y * y / 6.75) / gain + even * 0.25
            }
            // Crisp: harder clip for bright, edgy upper harmonics
            3 => (x * gain).clamp(-0.8, 0.8) / gain + even * 0.25,
            _ => (x * gain).tanh() / gain + even,
        }
    }

    pub fn process_block(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.enabled || self.mix <= 0.001 {
            return;
        }
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let orig_l = *l;
            let orig_r = *r;

            let (high_l, high_r) = self.high_crossover.process_sample(orig_l, orig_r);
            let excited_l = self.saturate(high_l);
            let excited_r = self.saturate(high_r);

            *l = orig_l + (excited_l - high_l) * self.mix;
            *r = orig_r + (excited_r - high_r) * self.mix;
        }
    }
}

/// Default signal flow; every module appears exactly once, and only enabled ones process
pub const DEFAULT_CHAIN: &[ModuleKind] = &[
    ModuleKind::MatchEq,
    ModuleKind::Equalizer,
    ModuleKind::PassiveEq,
    ModuleKind::StemRebalance,
    ModuleKind::StemEq,
    ModuleKind::LowEnd,
    ModuleKind::BassEnhancer,
    ModuleKind::Dynamics,
    ModuleKind::VintageCompressor,
    ModuleKind::Transient,
    ModuleKind::TransientRestore,
    ModuleKind::Imager,
    ModuleKind::Exciter,
    ModuleKind::Tape,
    ModuleKind::Clarity,
    ModuleKind::SpectralShaper,
    ModuleKind::Stabilizer,
    ModuleKind::OptoLimiter,
    ModuleKind::Maximizer,
];

/// Per-module loudness compensation: measures loudness before and after a module and
/// trims its output so switching or tweaking the module changes tone, not level
#[derive(Clone, Debug)]
pub struct AutoGain {
    pre: LoudnessFollower,
    post: LoudnessFollower,
    gain: f32,
}

impl AutoGain {
    const MAX_DB: f32 = 12.0;

    fn new(sample_rate: f32) -> Self {
        Self { pre: LoudnessFollower::new(sample_rate, 3.0), post: LoudnessFollower::new(sample_rate, 3.0), gain: 1.0 }
    }

    fn set_sample_rate(&mut self, sr: f32) {
        self.pre.set_sample_rate(sr);
        self.post.set_sample_rate(sr);
        self.gain = 1.0;
    }

    fn reset(&mut self) {
        self.pre.reset();
        self.post.reset();
        self.gain = 1.0;
    }

    /// Current compensation in dB
    pub fn gain_db(&self) -> f32 {
        20.0 * self.gain.max(1e-6).log10()
    }

    /// Ramp the output toward the measured compensation over ~300 ms
    fn apply(&mut self, left: &mut [f32], right: &mut [f32], sample_rate: f32) {
        let n = left.len().min(right.len());
        if n == 0 {
            return;
        }
        let target = if self.pre.is_active() && self.post.is_active() {
            let db = (self.pre.lufs() - self.post.lufs()).clamp(-Self::MAX_DB, Self::MAX_DB);
            10.0f32.powf(db / 20.0)
        } else {
            self.gain
        };
        let c = (-(n as f32) / (0.3 * sample_rate.max(1.0))).exp();
        let next = target + c * (self.gain - target);
        let step = (next - self.gain) / n as f32;
        for i in 0..n {
            let g = self.gain + step * (i + 1) as f32;
            left[i] *= g;
            right[i] *= g;
        }
        self.gain = next;
    }
}

/// Most shared spectral passes a chain can form (five spectral modules, two or more per pass)
const MAX_SPECTRAL_PASSES: usize = 2;

/// State of the shared spectral passes: neighbouring spectral modules (only switched-off modules
/// between them) are analysed and resynthesised once, by one STFT, instead of once each
#[derive(Clone, Debug)]
struct SpectralPasses {
    stft: [stft::StftProcessor; MAX_SPECTRAL_PASSES],
    /// Stem masks computed once per frame for the stem modules in a pass
    masker: [StemMasker; MAX_SPECTRAL_PASSES],
    /// Members of each pass (bit per `ModuleKind::index`) when it last ran
    members: [u32; MAX_SPECTRAL_PASSES],
    /// Every module that ran inside a pass during the last block
    grouped: u32,
}

impl SpectralPasses {
    fn new() -> Self {
        Self {
            stft: [stft::StftProcessor::new(), stft::StftProcessor::new()],
            masker: [StemMasker::default(), StemMasker::default()],
            members: [0; MAX_SPECTRAL_PASSES],
            grouped: 0,
        }
    }

    fn reset(&mut self) {
        for i in 0..MAX_SPECTRAL_PASSES {
            self.stft[i].reset();
            self.masker[i].reset();
        }
    }
}

/// One processing step of the chain
#[derive(Clone, Copy)]
enum ChainStep {
    /// A module on its own, at chain position `pos`
    Single(usize),
    /// A shared spectral pass over the active spectral modules in chain positions `start..end`
    Pass { start: usize, end: usize },
}

/// The complete built-in mastering suite: an ordered chain of modules plus metering
#[derive(Clone, Debug)]
pub struct MasteringSuite {
    pub dynamic_eq: DynamicEqualizer,
    pub passive_eq: PassiveEq,
    pub match_eq: MatchEq,
    pub stem_eq: StemEq,
    pub stem_rebalance: StemRebalance,
    pub low_end: LowEndShaper,
    pub bass: BassEnhancer,
    pub multiband_dynamics: MultibandDynamics,
    pub vintage_comp: VintageCompressor,
    pub transient: MultibandTransient,
    pub transient_restore: TransientRestore,
    pub imager: MasteringImager,
    pub exciter: MasteringExciter,
    pub tape: TapeSaturation,
    pub clarity: SpectralClarity,
    pub spectral_shaper: SpectralShaper,
    pub stabilizer: TonalStabilizer,
    pub opto_limiter: OptoLimiter,
    pub maximizer: MasteringMaximizer,
    /// Processing order
    pub chain: Vec<ModuleKind>,
    /// Per-module auto gain-match switches, indexed by `ModuleKind::index`
    pub auto_gain: [bool; ModuleKind::COUNT],
    /// Oversampling factor per module (1 = off), indexed by `ModuleKind::index`
    pub oversampling: [usize; ModuleKind::COUNT],
    oversamplers: Vec<oversampling::Oversampler>,
    /// Set by the host while separated stems are being mixed in front of the chain (applying
    /// Stem Rebalance / Stem EQ to the real stems), so the real-time approximations step aside
    pub external_stems: bool,
    auto_gain_state: Vec<AutoGain>,
    spectral_passes: Option<Box<SpectralPasses>>,
    pub lufs_meter: EbuR128Meter,
    pub spectral_analyzer: SpectralAnalyzer,
    pub sample_rate: f32,
}

impl MasteringSuite {
    pub fn new(sample_rate: f32) -> Self {
        let mut suite = Self {
            dynamic_eq: DynamicEqualizer::new(sample_rate),
            passive_eq: PassiveEq::new(sample_rate),
            match_eq: MatchEq::new(sample_rate),
            stem_eq: StemEq::new(sample_rate),
            stem_rebalance: StemRebalance::new(sample_rate),
            low_end: LowEndShaper::new(sample_rate),
            bass: BassEnhancer::new(sample_rate),
            multiband_dynamics: MultibandDynamics::new(sample_rate),
            vintage_comp: VintageCompressor::new(sample_rate),
            transient: MultibandTransient::new(sample_rate),
            transient_restore: TransientRestore::new(sample_rate),
            imager: MasteringImager::new(sample_rate),
            exciter: MasteringExciter::new(sample_rate),
            tape: TapeSaturation::new(sample_rate),
            clarity: SpectralClarity::new(sample_rate),
            spectral_shaper: SpectralShaper::new(sample_rate),
            stabilizer: TonalStabilizer::new(sample_rate),
            opto_limiter: OptoLimiter::new(sample_rate),
            maximizer: MasteringMaximizer::new(sample_rate),
            chain: DEFAULT_CHAIN.to_vec(),
            auto_gain: [false; ModuleKind::COUNT],
            oversampling: [1; ModuleKind::COUNT],
            oversamplers: (0..ModuleKind::COUNT).map(|_| oversampling::Oversampler::new(1)).collect(),
            external_stems: false,
            auto_gain_state: vec![AutoGain::new(sample_rate); ModuleKind::COUNT],
            spectral_passes: Some(Box::new(SpectralPasses::new())),
            lufs_meter: EbuR128Meter::new(sample_rate),
            spectral_analyzer: SpectralAnalyzer::new(sample_rate),
            sample_rate,
        };
        for &kind in ModuleKind::ALL {
            let factor = suite.module(kind).oversampling_default().max(1);
            suite.set_oversampling(kind, factor);
        }
        suite
    }

    pub fn module(&self, kind: ModuleKind) -> &dyn Module {
        match kind {
            ModuleKind::Equalizer => &self.dynamic_eq,
            ModuleKind::PassiveEq => &self.passive_eq,
            ModuleKind::MatchEq => &self.match_eq,
            ModuleKind::StemEq => &self.stem_eq,
            ModuleKind::StemRebalance => &self.stem_rebalance,
            ModuleKind::LowEnd => &self.low_end,
            ModuleKind::BassEnhancer => &self.bass,
            ModuleKind::Dynamics => &self.multiband_dynamics,
            ModuleKind::VintageCompressor => &self.vintage_comp,
            ModuleKind::Transient => &self.transient,
            ModuleKind::TransientRestore => &self.transient_restore,
            ModuleKind::Imager => &self.imager,
            ModuleKind::Exciter => &self.exciter,
            ModuleKind::Tape => &self.tape,
            ModuleKind::Clarity => &self.clarity,
            ModuleKind::SpectralShaper => &self.spectral_shaper,
            ModuleKind::Stabilizer => &self.stabilizer,
            ModuleKind::OptoLimiter => &self.opto_limiter,
            ModuleKind::Maximizer => &self.maximizer,
        }
    }

    pub fn module_mut(&mut self, kind: ModuleKind) -> &mut dyn Module {
        match kind {
            ModuleKind::Equalizer => &mut self.dynamic_eq,
            ModuleKind::PassiveEq => &mut self.passive_eq,
            ModuleKind::MatchEq => &mut self.match_eq,
            ModuleKind::StemEq => &mut self.stem_eq,
            ModuleKind::StemRebalance => &mut self.stem_rebalance,
            ModuleKind::LowEnd => &mut self.low_end,
            ModuleKind::BassEnhancer => &mut self.bass,
            ModuleKind::Dynamics => &mut self.multiband_dynamics,
            ModuleKind::VintageCompressor => &mut self.vintage_comp,
            ModuleKind::Transient => &mut self.transient,
            ModuleKind::TransientRestore => &mut self.transient_restore,
            ModuleKind::Imager => &mut self.imager,
            ModuleKind::Exciter => &mut self.exciter,
            ModuleKind::Tape => &mut self.tape,
            ModuleKind::Clarity => &mut self.clarity,
            ModuleKind::SpectralShaper => &mut self.spectral_shaper,
            ModuleKind::Stabilizer => &mut self.stabilizer,
            ModuleKind::OptoLimiter => &mut self.opto_limiter,
            ModuleKind::Maximizer => &mut self.maximizer,
        }
    }

    /// Whether a module offers oversampling
    pub fn supports_oversampling(&self, kind: ModuleKind) -> bool {
        self.module(kind).oversampling_default() > 0
    }

    /// Run a module at 1x, 2x or 4x the suite rate (ignored for modules that don't offer it)
    pub fn set_oversampling(&mut self, kind: ModuleKind, factor: usize) {
        let factor = match factor {
            2 => 2,
            f if f >= 4 => 4,
            _ => 1,
        };
        if !self.supports_oversampling(kind) && factor > 1 {
            return;
        }
        let idx = kind.index();
        let sr = self.sample_rate;
        self.oversampling[idx] = factor;
        self.oversamplers[idx] = oversampling::Oversampler::new(factor);
        self.module_mut(kind).set_sample_rate(sr * factor as f32);
    }

    /// Switch a module's auto gain-match on or off (restarting its measurement)
    pub fn set_auto_gain(&mut self, kind: ModuleKind, on: bool) {
        let idx = kind.index();
        if self.auto_gain[idx] != on {
            self.auto_gain[idx] = on;
            self.auto_gain_state[idx].reset();
        }
    }

    /// Current auto gain-match compensation of a module in dB
    pub fn auto_gain_db(&self, kind: ModuleKind) -> f32 {
        self.auto_gain_state[kind.index()].gain_db()
    }

    /// Mirror a compensation value measured elsewhere (the UI copy shows the audio thread's)
    pub fn show_auto_gain_db(&mut self, kind: ModuleKind, db: f32) {
        self.auto_gain_state[kind.index()].gain = 10.0f32.powf(db / 20.0);
    }

    /// Move a module `delta` slots in the chain (negative = earlier)
    pub fn move_module(&mut self, kind: ModuleKind, delta: i32) {
        if let Some(pos) = self.chain.iter().position(|k| *k == kind) {
            let target = (pos as i32 + delta).clamp(0, self.chain.len() as i32 - 1) as usize;
            let item = self.chain.remove(pos);
            self.chain.insert(target, item);
        }
    }

    /// Replace the chain order, accepting it only if it lists every module exactly once
    pub fn set_chain(&mut self, order: Vec<ModuleKind>) -> bool {
        let complete = order.len() == ModuleKind::ALL.len() && ModuleKind::ALL.iter().all(|k| order.contains(k));
        if complete {
            self.chain = order;
        }
        complete
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for &kind in ModuleKind::ALL {
            let factor = self.oversampling[kind.index()];
            self.module_mut(kind).set_sample_rate(sample_rate * factor as f32);
        }
        for os in self.oversamplers.iter_mut() {
            os.reset();
        }
        for ag in self.auto_gain_state.iter_mut() {
            ag.set_sample_rate(sample_rate);
        }
        self.lufs_meter.set_sample_rate(sample_rate);
        self.spectral_analyzer.set_sample_rate(sample_rate);
    }

    pub fn reset_state(&mut self) {
        for &kind in ModuleKind::ALL {
            self.module_mut(kind).reset();
        }
        for os in self.oversamplers.iter_mut() {
            os.reset();
        }
        for ag in self.auto_gain_state.iter_mut() {
            ag.reset();
        }
        if let Some(passes) = self.spectral_passes.as_mut() {
            passes.reset();
        }
        self.lufs_meter.reset();
        self.spectral_analyzer.reset();
    }

    /// Processing delay of the chain in samples: each module on its own adds its latency, and a
    /// shared spectral pass adds one STFT length however many modules it holds
    pub fn latency_samples(&self) -> usize {
        let mut total = 0;
        let mut pos = 0;
        while let Some((step, next)) = self.next_step(pos) {
            total += match step {
                ChainStep::Single(p) => {
                    // A module's own latency is counted at its (possibly oversampled) rate
                    let k = self.chain[p];
                    let factor = self.oversampling[k.index()];
                    self.module(k).latency() / factor + self.oversamplers[k.index()].latency()
                }
                ChainStep::Pass { .. } => stft::STFT_SIZE,
            };
            pos = next;
        }
        total
    }

    /// Number of modules sharing a spectral pass (shown in the editor)
    pub fn shared_spectral_modules(&self) -> usize {
        let mut count = 0;
        let mut pos = 0;
        while let Some((step, next)) = self.next_step(pos) {
            if let ChainStep::Pass { start, end } = step {
                count += (start..end).filter(|&p| self.is_active(self.chain[p])).count();
            }
            pos = next;
        }
        count
    }

    /// Whether a module processes this block
    fn is_active(&self, kind: ModuleKind) -> bool {
        self.module(kind).enabled() && !(self.external_stems && Self::is_stem_module(kind))
    }

    /// Whether an active module can join a shared spectral pass. Auto gain-match measures a
    /// module's own input and output, so those modules keep their own STFT.
    fn joins_pass(&self, kind: ModuleKind) -> bool {
        self.module(kind).is_spectral() && !self.auto_gain[kind.index()] && self.oversampling[kind.index()] == 1
    }

    /// The processing step starting at chain position `pos` or later, and where the next one starts
    fn next_step(&self, mut pos: usize) -> Option<(ChainStep, usize)> {
        while pos < self.chain.len() && !self.is_active(self.chain[pos]) {
            pos += 1;
        }
        if pos >= self.chain.len() {
            return None;
        }
        if !self.joins_pass(self.chain[pos]) {
            return Some((ChainStep::Single(pos), pos + 1));
        }
        let mut members = 1;
        let mut end = pos + 1;
        for p in pos + 1..self.chain.len() {
            let k = self.chain[p];
            if !self.is_active(k) {
                continue;
            }
            if !self.joins_pass(k) {
                break;
            }
            members += 1;
            end = p + 1;
        }
        Some(if members >= 2 { (ChainStep::Pass { start: pos, end }, end) } else { (ChainStep::Single(pos), pos + 1) })
    }

    fn is_stem_module(kind: ModuleKind) -> bool {
        matches!(kind, ModuleKind::StemRebalance | ModuleKind::StemEq)
    }

    /// Whether Stem Rebalance or Stem EQ is switched on
    pub fn uses_stems(&self) -> bool {
        self.stem_rebalance.enabled || self.stem_eq.enabled
    }

    /// Process audio through the mastering chain. Dither is applied at export time by the renderer.
    pub fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        let mut passes = self.spectral_passes.take().unwrap_or_else(|| Box::new(SpectralPasses::new()));
        self.regroup_spectral(&mut passes);

        let mut pos = 0;
        let mut pass_index = 0;
        while let Some((step, next)) = self.next_step(pos) {
            match step {
                ChainStep::Single(p) => self.process_module(self.chain[p], left, right),
                ChainStep::Pass { start, end } => {
                    self.process_pass(&mut passes, pass_index.min(MAX_SPECTRAL_PASSES - 1), start, end, left, right);
                    pass_index += 1;
                }
            }
            pos = next;
        }
        self.spectral_passes = Some(passes);

        // Feed EBU R128 Loudness Meter & Real-Time Spectral Analyzer
        self.lufs_meter.process_block(left, right);
        self.spectral_analyzer.feed_samples(left, right);
    }

    /// Restart passes whose membership changed, and reset modules that moved into or out of a
    /// pass (their own STFT, or the pass's, holds audio from before the change)
    fn regroup_spectral(&mut self, passes: &mut SpectralPasses) {
        let mut members = [0u32; MAX_SPECTRAL_PASSES];
        let mut grouped = 0u32;
        let mut pos = 0;
        let mut pass_index = 0;
        while let Some((step, next)) = self.next_step(pos) {
            if let ChainStep::Pass { start, end } = step {
                let slot = pass_index.min(MAX_SPECTRAL_PASSES - 1);
                for p in start..end {
                    let k = self.chain[p];
                    if self.is_active(k) {
                        members[slot] |= 1 << k.index();
                    }
                }
                grouped |= members[slot];
                pass_index += 1;
            }
            pos = next;
        }
        for slot in 0..MAX_SPECTRAL_PASSES {
            if members[slot] != passes.members[slot] {
                passes.stft[slot].reset();
                passes.masker[slot].reset();
                passes.members[slot] = members[slot];
            }
        }
        let moved = grouped ^ passes.grouped;
        if moved != 0 {
            for &kind in ModuleKind::ALL {
                if moved & (1 << kind.index()) != 0 {
                    self.module_mut(kind).reset();
                }
            }
            passes.grouped = grouped;
        }
    }

    /// Run one module on its own, with oversampling and auto gain-match if switched on
    fn process_module(&mut self, kind: ModuleKind, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let idx = kind.index();
        let auto = self.auto_gain[idx];
        if auto {
            self.auto_gain_state[idx].pre.feed(left, right);
        }
        if self.oversampling[idx] > 1 {
            // Borrow the oversampler out (an empty 1x one allocates nothing) so the closure
            // can borrow the module
            let mut os = std::mem::replace(&mut self.oversamplers[idx], oversampling::Oversampler::new(1));
            os.process(left, right, &mut |l, r| self.module_mut(kind).process(l, r));
            self.oversamplers[idx] = os;
            // Filter ringing from downsampling can poke past a limiter's ceiling
            if let Some(ceiling) = self.module(kind).output_ceiling() {
                for v in left.iter_mut().chain(right.iter_mut()) {
                    *v = v.clamp(-ceiling, ceiling);
                }
            }
        } else {
            self.module_mut(kind).process(left, right);
        }
        if auto {
            let ag = &mut self.auto_gain_state[idx];
            ag.post.feed(left, right);
            ag.apply(left, right, sr);
        }
    }

    /// Run the active spectral modules in chain positions `start..end` through one shared STFT
    fn process_pass(&mut self, passes: &mut SpectralPasses, slot: usize, start: usize, end: usize, left: &mut [f32], right: &mut [f32]) {
        let mut members = [ModuleKind::Equalizer; ModuleKind::COUNT];
        let mut count = 0;
        let mut needs_masks = false;
        for p in start..end {
            let k = self.chain[p];
            if self.is_active(k) {
                members[count] = k;
                count += 1;
                needs_masks |= self.module_mut(k).spectral().is_some_and(|m| m.uses_stem_masks());
            }
        }
        let sr = self.sample_rate;
        let SpectralPasses { stft, masker, .. } = passes;
        let masker = &mut masker[slot];
        stft[slot].process(left, right, &mut |frame| {
            let analysis = if needs_masks {
                masker.analyze(frame, sr);
                stft::FrameAnalysis { stem_masks: Some(&masker.masks) }
            } else {
                stft::FrameAnalysis::default()
            };
            for &k in &members[..count] {
                if let Some(m) = self.module_mut(k).spectral() {
                    m.process_frame(frame, &analysis);
                }
            }
        });
    }
}
