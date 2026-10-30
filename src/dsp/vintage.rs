//! Analog-flavoured character modules: Vintage Compressor, Vintage Passive EQ,
//! Vintage Opto Limiter and Tape Saturation.

use super::module::{db_to_gain, gain_to_db, time_coeff, Envelope, Module, ParamInfo, ParamUnit};
use super::{BiquadFilter, FilterType};

// ---------------------------------------------------------------------------------------------

const COMP_MODES: &[&str] = &["Clean", "Warm", "Vari-Mu"];

/// Feedback-topology compressor with soft knee, program-dependent release, sidechain high-pass
/// and optional tube-style saturation
#[derive(Clone, Debug)]
pub struct VintageCompressor {
    pub enabled: bool,
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub sidechain_hp_hz: f32,
    pub makeup_db: f32,
    pub mix: f32,
    pub mode: usize,
    sidechain: BiquadFilter,
    env: Envelope,
    slow_env: Envelope,
    prev_out: (f32, f32),
    gain_reduction_db: f32,
    sample_rate: f32,
}

impl VintageCompressor {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            enabled: false,
            threshold_db: -18.0,
            ratio: 2.0,
            attack_ms: 10.0,
            release_ms: 150.0,
            sidechain_hp_hz: 80.0,
            makeup_db: 0.0,
            mix: 1.0,
            mode: 1,
            sidechain: BiquadFilter::new(FilterType::HighPass, 80.0, 0.707, 0.0, sample_rate),
            env: Envelope::default(),
            slow_env: Envelope::default(),
            prev_out: (0.0, 0.0),
            gain_reduction_db: 0.0,
            sample_rate,
        }
    }
}

impl Module for VintageCompressor {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        vec![
            ParamInfo::db("Threshold", -48.0, 0.0),
            ParamInfo::new("Ratio", ParamUnit::Ratio, 1.0, 20.0, 0.1),
            ParamInfo::new("Attack", ParamUnit::Ms, 0.1, 100.0, 0.5),
            ParamInfo::new("Release", ParamUnit::Ms, 20.0, 2000.0, 10.0),
            ParamInfo::hz("Sidechain HP", 20.0, 400.0),
            ParamInfo::db("Makeup", -6.0, 18.0),
            ParamInfo::percent("Mix (parallel)", 0.0, 1.0),
            ParamInfo::choice("Character", COMP_MODES),
        ]
    }
    fn get_param(&self, i: usize) -> f32 {
        [
            self.threshold_db, self.ratio, self.attack_ms, self.release_ms,
            self.sidechain_hp_hz, self.makeup_db, self.mix, self.mode as f32,
        ][i.min(7)]
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.threshold_db = v.clamp(-48.0, 0.0),
            1 => self.ratio = v.clamp(1.0, 20.0),
            2 => self.attack_ms = v.clamp(0.1, 100.0),
            3 => self.release_ms = v.clamp(20.0, 2000.0),
            4 => {
                self.sidechain_hp_hz = v.clamp(20.0, 400.0);
                self.sidechain.set_params(self.sidechain_hp_hz, 0.707, 0.0);
            }
            5 => self.makeup_db = v.clamp(-6.0, 18.0),
            6 => self.mix = v.clamp(0.0, 1.0),
            _ => self.mode = (v.round().max(0.0) as usize).min(2),
        }
    }
    fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        self.sidechain.set_sample_rate(sr);
    }
    fn reset(&mut self) {
        self.sidechain.reset_state();
        self.env = Envelope::default();
        self.slow_env = Envelope::default();
        self.prev_out = (0.0, 0.0);
    }
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let att = time_coeff(self.attack_ms, sr);
        let rel_fast = time_coeff(self.release_ms, sr);
        let rel_slow = time_coeff(self.release_ms * 6.0, sr);
        let knee = 6.0;
        let makeup = db_to_gain(self.makeup_db);
        // Vari-Mu: ratio rises with gain reduction
        let vari_mu = self.mode == 2;
        let drive = match self.mode { 0 => 0.0, 1 => 0.15, _ => 0.3 };
        let mut max_gr = 0.0f32;

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (dry_l, dry_r) = (*l, *r);
            // Feedback detection: the previous output drives the detector
            let (sl, sr_) = self.sidechain.process_sample(self.prev_out.0, self.prev_out.1);
            let level_db = gain_to_db(sl.abs().max(sr_.abs()) + 1e-9);

            let over = level_db - self.threshold_db;
            let ratio = if vari_mu { (1.0 + over.max(0.0) * 0.25).min(self.ratio.max(1.0)) } else { self.ratio };
            let gr_db = if over <= -knee / 2.0 {
                0.0
            } else if over < knee / 2.0 {
                (1.0 - 1.0 / ratio) * (over + knee / 2.0).powi(2) / (2.0 * knee)
            } else {
                (1.0 - 1.0 / ratio) * over
            };

            // Program-dependent release: sustained compression recovers on the slow envelope
            let fast = self.env.follow(gr_db, att, rel_fast);
            let slow = self.slow_env.follow(gr_db, att, rel_slow);
            let gr = fast.max(slow * 0.6);
            max_gr = max_gr.max(gr);

            let g = db_to_gain(-gr) * makeup;
            let mut wl = dry_l * g;
            let mut wr = dry_r * g;
            if drive > 0.0 {
                let k = 1.0 + drive * 4.0;
                wl = ((wl * k).tanh() + drive * 0.1 * (wl * wl)) / k * (1.0 + drive);
                wr = ((wr * k).tanh() + drive * 0.1 * (wr * wr)) / k * (1.0 + drive);
            }
            self.prev_out = (wl, wr);
            *l = dry_l * (1.0 - self.mix) + wl * self.mix;
            *r = dry_r * (1.0 - self.mix) + wr * self.mix;
        }
        self.gain_reduction_db = max_gr.max(self.gain_reduction_db * 0.9);
    }
    fn status(&self) -> Option<String> {
        Some(format!("Gain reduction: {:.1} dB", self.gain_reduction_db))
    }
}

// ---------------------------------------------------------------------------------------------

const PEQ_LOW_FREQS: &[&str] = &["20 Hz", "30 Hz", "60 Hz", "100 Hz"];
const PEQ_LOW_HZ: [f32; 4] = [20.0, 30.0, 60.0, 100.0];
const PEQ_HIGH_FREQS: &[&str] = &["3 kHz", "4 kHz", "5 kHz", "8 kHz", "10 kHz", "12 kHz", "16 kHz"];
const PEQ_HIGH_HZ: [f32; 7] = [3000.0, 4000.0, 5000.0, 8000.0, 10000.0, 12000.0, 16000.0];
const PEQ_ATTEN_FREQS: &[&str] = &["5 kHz", "10 kHz", "20 kHz"];
const PEQ_ATTEN_HZ: [f32; 3] = [5000.0, 10000.0, 20000.0];

/// Passive program EQ: simultaneous low boost and cut (the classic resonant low-end trick),
/// broad high boost, high shelf cut, and output transformer saturation
#[derive(Clone, Debug)]
pub struct PassiveEq {
    pub enabled: bool,
    pub low_freq: usize,
    pub low_boost_db: f32,
    pub low_atten_db: f32,
    pub high_freq: usize,
    pub high_boost_db: f32,
    pub high_bandwidth: f32,
    pub atten_freq: usize,
    pub high_atten_db: f32,
    pub drive: f32,
    low_boost: BiquadFilter,
    low_cut: BiquadFilter,
    high_boost: BiquadFilter,
    high_cut: BiquadFilter,
    sample_rate: f32,
}

impl PassiveEq {
    pub fn new(sample_rate: f32) -> Self {
        let mut eq = Self {
            enabled: false,
            low_freq: 2,
            low_boost_db: 0.0,
            low_atten_db: 0.0,
            high_freq: 4,
            high_boost_db: 0.0,
            high_bandwidth: 0.7,
            atten_freq: 1,
            high_atten_db: 0.0,
            drive: 0.0,
            low_boost: BiquadFilter::new(FilterType::LowShelf, 60.0, 0.7, 0.0, sample_rate),
            low_cut: BiquadFilter::new(FilterType::LowShelf, 96.0, 0.7, 0.0, sample_rate),
            high_boost: BiquadFilter::new(FilterType::Peaking, 10000.0, 0.7, 0.0, sample_rate),
            high_cut: BiquadFilter::new(FilterType::HighShelf, 10000.0, 0.7, 0.0, sample_rate),
            sample_rate,
        };
        eq.rebuild();
        eq
    }

    fn rebuild(&mut self) {
        let lf = PEQ_LOW_HZ[self.low_freq.min(3)];
        self.low_boost.set_params(lf, 0.6, self.low_boost_db * 1.4);
        // The cut sits a little above the boost, carving a dip above the bump
        self.low_cut.set_params(lf * 1.6, 0.6, -self.low_atten_db * 1.2);
        self.high_boost.set_params(PEQ_HIGH_HZ[self.high_freq.min(6)], self.high_bandwidth, self.high_boost_db * 1.3);
        self.high_cut.set_params(PEQ_ATTEN_HZ[self.atten_freq.min(2)], 0.7, -self.high_atten_db);
    }
}

impl Module for PassiveEq {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        vec![
            ParamInfo::choice("Low frequency", PEQ_LOW_FREQS),
            ParamInfo::new("Low boost", ParamUnit::Db, 0.0, 10.0, 0.5),
            ParamInfo::new("Low atten", ParamUnit::Db, 0.0, 10.0, 0.5),
            ParamInfo::choice("High boost freq", PEQ_HIGH_FREQS),
            ParamInfo::new("High boost", ParamUnit::Db, 0.0, 10.0, 0.5),
            ParamInfo::new("High bandwidth (Q)", ParamUnit::Plain, 0.3, 3.0, 0.05),
            ParamInfo::choice("High atten freq", PEQ_ATTEN_FREQS),
            ParamInfo::new("High atten", ParamUnit::Db, 0.0, 10.0, 0.5),
            ParamInfo::percent("Transformer drive", 0.0, 1.0),
        ]
    }
    fn get_param(&self, i: usize) -> f32 {
        [
            self.low_freq as f32, self.low_boost_db, self.low_atten_db, self.high_freq as f32,
            self.high_boost_db, self.high_bandwidth, self.atten_freq as f32, self.high_atten_db, self.drive,
        ][i.min(8)]
    }
    fn set_param(&mut self, i: usize, v: f32) {
        let idx = |v: f32, max: usize| (v.round().max(0.0) as usize).min(max);
        match i {
            0 => self.low_freq = idx(v, 3),
            1 => self.low_boost_db = v.clamp(0.0, 10.0),
            2 => self.low_atten_db = v.clamp(0.0, 10.0),
            3 => self.high_freq = idx(v, 6),
            4 => self.high_boost_db = v.clamp(0.0, 10.0),
            5 => self.high_bandwidth = v.clamp(0.3, 3.0),
            6 => self.atten_freq = idx(v, 2),
            7 => self.high_atten_db = v.clamp(0.0, 10.0),
            _ => self.drive = v.clamp(0.0, 1.0),
        }
        self.rebuild();
    }
    fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        for f in [&mut self.low_boost, &mut self.low_cut, &mut self.high_boost, &mut self.high_cut] {
            f.set_sample_rate(sr);
        }
        self.rebuild();
    }
    fn reset(&mut self) {
        for f in [&mut self.low_boost, &mut self.low_cut, &mut self.high_boost, &mut self.high_cut] {
            f.reset_state();
        }
    }
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let k = 1.0 + self.drive * 3.0;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut a, mut b) = self.low_boost.process_sample(*l, *r);
            (a, b) = self.low_cut.process_sample(a, b);
            (a, b) = self.high_boost.process_sample(a, b);
            (a, b) = self.high_cut.process_sample(a, b);
            if self.drive > 0.0 {
                // Transformer: gentle odd-order saturation with a touch of even harmonics
                a = ((a * k).tanh() + 0.05 * self.drive * a * a) / k;
                b = ((b * k).tanh() + 0.05 * self.drive * b * b) / k;
            }
            *l = a;
            *r = b;
        }
    }
}

// ---------------------------------------------------------------------------------------------

const LIMITER_CHARACTERS: &[&str] = &["Analog", "Tube", "Modern"];

/// Program-dependent limiter with a slower, smoother gain envelope than the maximizer and
/// selectable saturation colour, followed by a safety clip at the ceiling
#[derive(Clone, Debug)]
pub struct OptoLimiter {
    pub enabled: bool,
    pub drive_db: f32,
    pub ceiling_db: f32,
    pub release_ms: f32,
    pub character: usize,
    env: Envelope,
    slow_env: Envelope,
    gain_reduction_db: f32,
    sample_rate: f32,
}

impl OptoLimiter {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            enabled: false,
            drive_db: 3.0,
            ceiling_db: -0.3,
            release_ms: 300.0,
            character: 0,
            env: Envelope::default(),
            slow_env: Envelope::default(),
            gain_reduction_db: 0.0,
            sample_rate,
        }
    }
}

impl Module for OptoLimiter {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        vec![
            ParamInfo::db("Drive", 0.0, 18.0),
            ParamInfo::new("Ceiling", ParamUnit::Db, -12.0, 0.0, 0.1),
            ParamInfo::new("Release", ParamUnit::Ms, 30.0, 3000.0, 10.0),
            ParamInfo::choice("Character", LIMITER_CHARACTERS),
        ]
    }
    fn get_param(&self, i: usize) -> f32 {
        [self.drive_db, self.ceiling_db, self.release_ms, self.character as f32][i.min(3)]
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.drive_db = v.clamp(0.0, 18.0),
            1 => self.ceiling_db = v.clamp(-12.0, 0.0),
            2 => self.release_ms = v.clamp(30.0, 3000.0),
            _ => self.character = (v.round().max(0.0) as usize).min(2),
        }
    }
    fn set_sample_rate(&mut self, sr: f32) { self.sample_rate = sr; }
    fn reset(&mut self) {
        self.env = Envelope::default();
        self.slow_env = Envelope::default();
    }
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let (attack_ms, sat) = match self.character { 0 => (5.0, 0.2), 1 => (8.0, 0.45), _ => (1.0, 0.0) };
        let att = time_coeff(attack_ms, sr);
        let rel = time_coeff(self.release_ms, sr);
        let rel_slow = time_coeff(self.release_ms * 5.0, sr);
        let drive = db_to_gain(self.drive_db);
        let ceiling = db_to_gain(self.ceiling_db);
        let mut max_gr = 0.0f32;

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mut a = *l * drive;
            let mut b = *r * drive;
            if sat > 0.0 {
                // Tube adds asymmetry (even harmonics); Analog stays mostly odd-order
                let even = if self.character == 1 { 0.15 } else { 0.03 };
                a = (a * (1.0 + sat)).tanh() / (1.0 + sat) * (1.0 + sat * 0.5) + even * a * a.abs() * sat;
                b = (b * (1.0 + sat)).tanh() / (1.0 + sat) * (1.0 + sat * 0.5) + even * b * b.abs() * sat;
            }
            let peak = a.abs().max(b.abs());
            let over_db = (gain_to_db(peak + 1e-9) - self.ceiling_db).max(0.0);
            let fast = self.env.follow(over_db, att, rel);
            let slow = self.slow_env.follow(over_db, att, rel_slow);
            let gr = fast.max(slow * 0.7);
            max_gr = max_gr.max(gr);
            let g = db_to_gain(-gr);
            *l = (a * g).clamp(-ceiling, ceiling);
            *r = (b * g).clamp(-ceiling, ceiling);
        }
        self.gain_reduction_db = max_gr.max(self.gain_reduction_db * 0.9);
    }
    fn status(&self) -> Option<String> {
        Some(format!("Gain reduction: {:.1} dB", self.gain_reduction_db))
    }
}

// ---------------------------------------------------------------------------------------------

const TAPE_SPEEDS: &[&str] = &["15 ips", "30 ips"];

/// Tape machine emulation: pre/de-emphasis around a hysteresis-flavoured saturation curve,
/// speed-dependent head bump and high-frequency roll-off
#[derive(Clone, Debug)]
pub struct TapeSaturation {
    pub enabled: bool,
    pub drive_db: f32,
    pub speed: usize,
    pub bias: f32,
    pub low_emphasis_db: f32,
    pub high_emphasis_db: f32,
    pub mix: f32,
    pre_emph: BiquadFilter,
    de_emph: BiquadFilter,
    head_bump: BiquadFilter,
    rolloff: BiquadFilter,
    hyst: (f32, f32),
    sample_rate: f32,
}

impl TapeSaturation {
    pub fn new(sample_rate: f32) -> Self {
        let mut t = Self {
            enabled: false,
            drive_db: 3.0,
            speed: 1,
            bias: 0.5,
            low_emphasis_db: 1.5,
            high_emphasis_db: 0.0,
            mix: 1.0,
            pre_emph: BiquadFilter::new(FilterType::HighShelf, 3000.0, 0.7, 6.0, sample_rate),
            de_emph: BiquadFilter::new(FilterType::HighShelf, 3000.0, 0.7, -6.0, sample_rate),
            head_bump: BiquadFilter::new(FilterType::Peaking, 60.0, 1.0, 1.5, sample_rate),
            rolloff: BiquadFilter::new(FilterType::LowPass, 20000.0, 0.707, 0.0, sample_rate),
            hyst: (0.0, 0.0),
            sample_rate,
        };
        t.rebuild();
        t
    }

    fn rebuild(&mut self) {
        let (bump_hz, roll_hz): (f32, f32) = if self.speed == 0 { (100.0, 16000.0) } else { (60.0, 21000.0) };
        self.head_bump.set_params(bump_hz, 1.0, self.low_emphasis_db);
        self.rolloff.set_params(roll_hz.min(self.sample_rate * 0.45), 0.707, 0.0);
        // High emphasis tilts the de-emphasis, brightening or darkening the return
        self.pre_emph.set_params(3000.0, 0.7, 6.0);
        self.de_emph.set_params(3000.0, 0.7, -6.0 + self.high_emphasis_db);
    }
}

impl Module for TapeSaturation {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        vec![
            ParamInfo::db("Input drive", 0.0, 18.0),
            ParamInfo::choice("Tape speed", TAPE_SPEEDS),
            ParamInfo::percent("Bias", 0.0, 1.0),
            ParamInfo::db("Low emphasis (head bump)", -6.0, 6.0),
            ParamInfo::db("High emphasis", -6.0, 6.0),
            ParamInfo::percent("Mix", 0.0, 1.0),
        ]
    }
    fn get_param(&self, i: usize) -> f32 {
        [self.drive_db, self.speed as f32, self.bias, self.low_emphasis_db, self.high_emphasis_db, self.mix][i.min(5)]
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.drive_db = v.clamp(0.0, 18.0),
            1 => self.speed = (v.round().max(0.0) as usize).min(1),
            2 => self.bias = v.clamp(0.0, 1.0),
            3 => self.low_emphasis_db = v.clamp(-6.0, 6.0),
            4 => self.high_emphasis_db = v.clamp(-6.0, 6.0),
            _ => self.mix = v.clamp(0.0, 1.0),
        }
        self.rebuild();
    }
    fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        for f in [&mut self.pre_emph, &mut self.de_emph, &mut self.head_bump, &mut self.rolloff] {
            f.set_sample_rate(sr);
        }
        self.rebuild();
    }
    fn reset(&mut self) {
        for f in [&mut self.pre_emph, &mut self.de_emph, &mut self.head_bump, &mut self.rolloff] {
            f.reset_state();
        }
        self.hyst = (0.0, 0.0);
    }
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let drive = db_to_gain(self.drive_db);
        // Lower bias = more asymmetric, grittier saturation; memory term gives a hysteresis lag
        let asym = (1.0 - self.bias) * 0.2;
        let memory = 0.15;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (dry_l, dry_r) = (*l, *r);
            let (pl, pr) = self.pre_emph.process_sample(dry_l * drive, dry_r * drive);
            let sat = |x: f32, prev: f32| {
                let y = x + memory * (x - prev);
                let s = if y.abs() < 1.5 { y - y * y * y / 6.75 } else { y.signum() * 1.0 };
                s + asym * s * s
            };
            let sl = sat(pl, self.hyst.0);
            let sr_ = sat(pr, self.hyst.1);
            self.hyst = (pl, pr);
            let (mut a, mut b) = self.de_emph.process_sample(sl / drive.sqrt(), sr_ / drive.sqrt());
            (a, b) = self.head_bump.process_sample(a, b);
            (a, b) = self.rolloff.process_sample(a, b);
            *l = dry_l * (1.0 - self.mix) + a * self.mix;
            *r = dry_r * (1.0 - self.mix) + b * self.mix;
        }
    }
}
