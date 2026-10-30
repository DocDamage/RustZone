//! Time-domain dynamics shapers: Low-End Shaper, Multiband Transient Shaper, Transient Restore
//! and Bass Enhancer. Transients are detected as the ratio between a fast and a slow envelope.

use super::module::{db_to_gain, gain_to_db, time_coeff, Envelope, Module, ParamInfo, ParamUnit};
use super::multiband::LinkwitzRiley4;
use super::{BiquadFilter, FilterType};

/// Fast/slow envelope pair whose ratio rises above 1 on attacks and falls below 1 in sustains
#[derive(Clone, Copy, Debug, Default)]
struct TransientDetector {
    fast: Envelope,
    slow: Envelope,
}

impl TransientDetector {
    /// Returns fast/slow (≈1 for steady signals, >1 on attacks)
    #[inline]
    fn ratio(&mut self, x: f32, coeffs: &DetectorCoeffs) -> f32 {
        let a = x.abs();
        let f = self.fast.follow(a, coeffs.fast_att, coeffs.fast_rel);
        let s = self.slow.follow(a, coeffs.slow_att, coeffs.slow_rel);
        (f + 1e-6) / (s + 1e-6)
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Copy, Debug)]
struct DetectorCoeffs {
    fast_att: f32,
    fast_rel: f32,
    slow_att: f32,
    slow_rel: f32,
}

impl DetectorCoeffs {
    fn new(sample_rate: f32, speed: f32) -> Self {
        // speed scales every time constant (0.5 = twice as fast)
        Self {
            fast_att: time_coeff(0.5 * speed, sample_rate),
            fast_rel: time_coeff(30.0 * speed, sample_rate),
            slow_att: time_coeff(25.0 * speed, sample_rate),
            slow_rel: time_coeff(250.0 * speed, sample_rate),
        }
    }
}

/// Gain applied for a transient ratio and an emphasis amount (-1..1)
#[inline]
fn shape_gain(ratio: f32, amount: f32) -> f32 {
    ratio.max(1e-3).powf(amount * 1.5).clamp(0.25, 4.0)
}

// ---------------------------------------------------------------------------------------------

const LOW_END_MODES: &[&str] = &["Punchy", "Smooth"];

/// Shapes the low end only: positive contrast adds punch to kick/bass attacks, negative
/// contrast evens them out
#[derive(Clone, Debug)]
pub struct LowEndShaper {
    pub enabled: bool,
    pub cutoff_hz: f32,
    pub contrast: f32,
    pub gain_db: f32,
    pub mode: usize,
    split: LinkwitzRiley4,
    detector: TransientDetector,
    coeffs: DetectorCoeffs,
    sample_rate: f32,
}

impl LowEndShaper {
    pub fn new(sample_rate: f32) -> Self {
        let mut s = Self {
            enabled: false,
            cutoff_hz: 120.0,
            contrast: 0.3,
            gain_db: 0.0,
            mode: 0,
            split: LinkwitzRiley4::new(120.0, sample_rate),
            detector: TransientDetector::default(),
            coeffs: DetectorCoeffs::new(sample_rate, 1.0),
            sample_rate,
        };
        s.rebuild();
        s
    }

    fn rebuild(&mut self) {
        self.split = LinkwitzRiley4::new(self.cutoff_hz, self.sample_rate);
        // Smooth mode listens over longer windows
        self.coeffs = DetectorCoeffs::new(self.sample_rate, if self.mode == 1 { 2.0 } else { 1.0 });
    }
}

impl Module for LowEndShaper {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        vec![
            ParamInfo::hz("Cutoff", 40.0, 300.0),
            ParamInfo::percent("Contrast", -1.0, 1.0),
            ParamInfo::db("Low gain", -12.0, 12.0),
            ParamInfo::choice("Mode", LOW_END_MODES),
        ]
    }
    fn get_param(&self, i: usize) -> f32 {
        match i { 0 => self.cutoff_hz, 1 => self.contrast, 2 => self.gain_db, _ => self.mode as f32 }
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.cutoff_hz = v.clamp(40.0, 300.0),
            1 => self.contrast = v.clamp(-1.0, 1.0),
            2 => self.gain_db = v.clamp(-12.0, 12.0),
            _ => self.mode = (v.round().max(0.0) as usize).min(1),
        }
        if i == 0 || i == 3 { self.rebuild(); }
    }
    fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        self.rebuild();
    }
    fn reset(&mut self) {
        self.split.reset();
        self.detector.reset();
    }
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let g = db_to_gain(self.gain_db);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let ((ll, lr), (hl, hr)) = self.split.split(*l, *r);
            let ratio = self.detector.ratio(0.5 * (ll + lr), &self.coeffs);
            let k = shape_gain(ratio, self.contrast) * g;
            *l = hl + ll * k;
            *r = hr + lr * k;
        }
    }
}

// ---------------------------------------------------------------------------------------------

const TRANSIENT_BANDS: [&str; 4] = ["Low", "Low-mid", "High-mid", "High"];

/// Four-band transient shaper: per band, positive punch accents attacks, negative softens them
#[derive(Clone, Debug)]
pub struct MultibandTransient {
    pub enabled: bool,
    pub punch: [f32; 4],
    pub crossovers_hz: [f32; 3],
    splits: [LinkwitzRiley4; 3],
    detectors: [TransientDetector; 4],
    coeffs: DetectorCoeffs,
    sample_rate: f32,
}

impl MultibandTransient {
    pub fn new(sample_rate: f32) -> Self {
        let mut t = Self {
            enabled: false,
            punch: [0.0; 4],
            crossovers_hz: [150.0, 1500.0, 6000.0],
            splits: [LinkwitzRiley4::new(150.0, sample_rate); 3],
            detectors: [TransientDetector::default(); 4],
            coeffs: DetectorCoeffs::new(sample_rate, 0.6),
            sample_rate,
        };
        t.rebuild();
        t
    }

    fn rebuild(&mut self) {
        for i in 0..3 {
            self.splits[i] = LinkwitzRiley4::new(self.crossovers_hz[i], self.sample_rate);
        }
        self.coeffs = DetectorCoeffs::new(self.sample_rate, 0.6);
    }
}

impl Module for MultibandTransient {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        let mut p: Vec<ParamInfo> = TRANSIENT_BANDS.iter().map(|b| ParamInfo::percent(format!("{} punch", b), -1.0, 1.0)).collect();
        p.push(ParamInfo::hz("Crossover 1", 40.0, 500.0));
        p.push(ParamInfo::hz("Crossover 2", 500.0, 4000.0));
        p.push(ParamInfo::hz("Crossover 3", 2000.0, 16000.0));
        p
    }
    fn get_param(&self, i: usize) -> f32 {
        if i < 4 { self.punch[i] } else { self.crossovers_hz[(i - 4).min(2)] }
    }
    fn set_param(&mut self, i: usize, v: f32) {
        if i < 4 {
            self.punch[i] = v.clamp(-1.0, 1.0);
        } else {
            let c = (i - 4).min(2);
            let lo = if c == 0 { 40.0 } else { self.crossovers_hz[c - 1] * 1.2 };
            let hi = if c == 2 { 16000.0 } else { self.crossovers_hz[c + 1] / 1.2 };
            self.crossovers_hz[c] = v.clamp(lo, hi.max(lo));
            self.rebuild();
        }
    }
    fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        self.rebuild();
    }
    fn reset(&mut self) {
        for s in self.splits.iter_mut() { s.reset(); }
        for d in self.detectors.iter_mut() { d.reset(); }
    }
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.punch.iter().all(|p| p.abs() < 1e-4) {
            return; // neutral settings: leave the signal untouched (no crossover phase shift)
        }
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            // Successive splits: band 0 = below c1, band 1 = c1..c2, band 2 = c2..c3, band 3 = above c3
            let (b0, rest) = self.splits[0].split(*l, *r);
            let (b1, rest) = self.splits[1].split(rest.0, rest.1);
            let (b2, b3) = self.splits[2].split(rest.0, rest.1);
            let bands = [b0, b1, b2, b3];
            let (mut ol, mut or) = (0.0, 0.0);
            for (i, (bl, br)) in bands.iter().enumerate() {
                let k = if self.punch[i].abs() < 1e-4 {
                    1.0
                } else {
                    shape_gain(self.detectors[i].ratio(0.5 * (bl + br), &self.coeffs), self.punch[i])
                };
                ol += bl * k;
                or += br * k;
            }
            *l = ol;
            *r = or;
        }
    }
}

// ---------------------------------------------------------------------------------------------

/// Restores attacks flattened by heavy limiting by expanding detected transients upward
#[derive(Clone, Debug)]
pub struct TransientRestore {
    pub enabled: bool,
    pub amount: f32,
    pub sensitivity_db: f32,
    pub speed: f32,
    pub max_gain_db: f32,
    detector: TransientDetector,
    coeffs: DetectorCoeffs,
    gain_env: Envelope,
    last_gain_db: f32,
    sample_rate: f32,
}

impl TransientRestore {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            enabled: false,
            amount: 0.5,
            sensitivity_db: -30.0,
            speed: 1.0,
            max_gain_db: 6.0,
            detector: TransientDetector::default(),
            coeffs: DetectorCoeffs::new(sample_rate, 0.5),
            gain_env: Envelope { value: 1.0 },
            last_gain_db: 0.0,
            sample_rate,
        }
    }
}

impl Module for TransientRestore {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        vec![
            ParamInfo::percent("Amount", 0.0, 1.0),
            ParamInfo::db("Sensitivity (floor)", -60.0, 0.0),
            ParamInfo::new("Speed", ParamUnit::Plain, 0.25, 4.0, 0.05),
            ParamInfo::db("Max boost", 1.0, 12.0),
        ]
    }
    fn get_param(&self, i: usize) -> f32 {
        [self.amount, self.sensitivity_db, self.speed, self.max_gain_db][i.min(3)]
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.amount = v.clamp(0.0, 1.0),
            1 => self.sensitivity_db = v.clamp(-60.0, 0.0),
            2 => {
                self.speed = v.clamp(0.25, 4.0);
                self.coeffs = DetectorCoeffs::new(self.sample_rate, 0.5 * self.speed);
            }
            _ => self.max_gain_db = v.clamp(1.0, 12.0),
        }
    }
    fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        self.coeffs = DetectorCoeffs::new(sr, 0.5 * self.speed);
    }
    fn reset(&mut self) {
        self.detector.reset();
        self.gain_env = Envelope { value: 1.0 };
    }
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let floor = db_to_gain(self.sensitivity_db);
        let max_g = db_to_gain(self.max_gain_db);
        let att = time_coeff(0.3, self.sample_rate);
        let rel = time_coeff(40.0 * self.speed, self.sample_rate);
        let mut peak_db = 0.0f32;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let x = l.abs().max(r.abs());
            let ratio = self.detector.ratio(x, &self.coeffs);
            let target = if self.detector.slow.value > floor && ratio > 1.0 {
                (1.0 + (ratio - 1.0) * self.amount * 2.0).min(max_g)
            } else {
                1.0
            };
            let g = self.gain_env.follow(target, att, rel);
            peak_db = peak_db.max(gain_to_db(g));
            *l *= g;
            *r *= g;
        }
        self.last_gain_db = peak_db.max(self.last_gain_db * 0.9);
    }
    fn status(&self) -> Option<String> {
        Some(format!("Transient boost: {:.1} dB", self.last_gain_db))
    }
    fn meters(&self) -> [f32; 4] {
        [self.last_gain_db, 0.0, 0.0, 0.0]
    }
    fn set_meters(&mut self, m: [f32; 4]) {
        self.last_gain_db = m[0];
    }
}

// ---------------------------------------------------------------------------------------------

/// Low-end control: tighten (compress) and boost the sub band, add upper harmonics so bass
/// translates on small speakers, and optionally fold it to mono
#[derive(Clone, Debug)]
pub struct BassEnhancer {
    pub enabled: bool,
    pub frequency_hz: f32,
    pub boost_db: f32,
    pub tightness: f32,
    pub harmonics: f32,
    pub mono: bool,
    split: LinkwitzRiley4,
    harmonic_hp: BiquadFilter,
    env: Envelope,
    sample_rate: f32,
}

impl BassEnhancer {
    pub fn new(sample_rate: f32) -> Self {
        let mut b = Self {
            enabled: false,
            frequency_hz: 100.0,
            boost_db: 0.0,
            tightness: 0.3,
            harmonics: 0.0,
            mono: true,
            split: LinkwitzRiley4::new(100.0, sample_rate),
            harmonic_hp: BiquadFilter::new(FilterType::HighPass, 100.0, 0.707, 0.0, sample_rate),
            env: Envelope::default(),
            sample_rate,
        };
        b.rebuild();
        b
    }

    fn rebuild(&mut self) {
        self.split = LinkwitzRiley4::new(self.frequency_hz, self.sample_rate);
        self.harmonic_hp = BiquadFilter::new(FilterType::HighPass, self.frequency_hz * 1.5, 0.707, 0.0, self.sample_rate);
    }
}

impl Module for BassEnhancer {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        vec![
            ParamInfo::hz("Frequency", 40.0, 200.0),
            ParamInfo::db("Sub boost", -6.0, 12.0),
            ParamInfo::percent("Tightness", 0.0, 1.0),
            ParamInfo::percent("Harmonics", 0.0, 1.0),
            ParamInfo::toggle("Mono low end"),
        ]
    }
    fn get_param(&self, i: usize) -> f32 {
        match i {
            0 => self.frequency_hz,
            1 => self.boost_db,
            2 => self.tightness,
            3 => self.harmonics,
            _ => if self.mono { 1.0 } else { 0.0 },
        }
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => {
                self.frequency_hz = v.clamp(40.0, 200.0);
                self.rebuild();
            }
            1 => self.boost_db = v.clamp(-6.0, 12.0),
            2 => self.tightness = v.clamp(0.0, 1.0),
            3 => self.harmonics = v.clamp(0.0, 1.0),
            _ => self.mono = v > 0.5,
        }
    }
    fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        self.rebuild();
    }
    fn reset(&mut self) {
        self.split.reset();
        self.harmonic_hp.reset_state();
        self.env = Envelope::default();
    }
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let boost = db_to_gain(self.boost_db);
        let att = time_coeff(10.0, self.sample_rate);
        let rel = time_coeff(120.0, self.sample_rate);
        // Tightness = compression ratio 1:1 .. 4:1 above -24 dBFS on the sub band
        let ratio = 1.0 + 3.0 * self.tightness;
        let threshold = db_to_gain(-24.0);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let ((mut ll, mut lr), (hl, hr)) = self.split.split(*l, *r);
            if self.mono {
                let m = 0.5 * (ll + lr);
                ll = m;
                lr = m;
            }
            let e = self.env.follow(ll.abs().max(lr.abs()), att, rel);
            let comp = if e > threshold { (e / threshold).powf(1.0 / ratio - 1.0) } else { 1.0 };
            let k = comp * boost;
            ll *= k;
            lr *= k;

            let (mut extra_l, mut extra_r) = (0.0, 0.0);
            if self.harmonics > 0.0 {
                // Saturation products of the sub band, kept above the band so they add audibility
                let drive = 4.0;
                let hl_ = (ll * drive).tanh() / drive - ll;
                let hr_ = (lr * drive).tanh() / drive - lr;
                let (fl, fr) = self.harmonic_hp.process_sample(hl_, hr_);
                extra_l = -fl * self.harmonics * 3.0;
                extra_r = -fr * self.harmonics * 3.0;
            }
            *l = hl + ll + extra_l;
            *r = hr + lr + extra_r;
        }
    }
}
