//! Spectral (STFT-domain) mastering modules: Match EQ, Spectral Shaper, Tonal Stabilizer,
//! and the stem-aware Stem Rebalance / Stem EQ, which share a real-time stem masker.

use super::module::{db_to_gain, gain_to_db, time_coeff, Module, ParamInfo, ParamUnit};
use super::stft::{bin_freq, SpectralFrame, StftProcessor, STFT_BINS, STFT_HOP, STFT_SIZE};
use crate::audio::{AudioBuffer, Radix2Fft};
use std::f32::consts::PI;

/// Soft 0→1 transition between `lo` and `hi`
#[inline]
fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Frames-per-second based smoothing coefficient for per-frame (hop rate) envelopes
#[inline]
fn frame_coeff(ms: f32, sample_rate: f32) -> f32 {
    time_coeff(ms, sample_rate / STFT_HOP as f32)
}

// ---------------------------------------------------------------------------------------------
// Match EQ
// ---------------------------------------------------------------------------------------------

const MATCH_POINTS_PER_OCT: f32 = 24.0;
const MATCH_F_MIN: f32 = 20.0;
const MATCH_F_MAX: f32 = 20000.0;
const MATCH_SMOOTHING: &[&str] = &["1/12 oct", "1/6 oct", "1/3 oct", "1/2 oct", "1 oct"];
const MATCH_SMOOTHING_OCT: [f32; 5] = [1.0 / 12.0, 1.0 / 6.0, 1.0 / 3.0, 0.5, 1.0];

fn match_point_count() -> usize {
    ((MATCH_F_MAX / MATCH_F_MIN).log2() * MATCH_POINTS_PER_OCT) as usize + 1
}

fn match_point_freq(p: usize) -> f32 {
    MATCH_F_MIN * 2.0f32.powf(p as f32 / MATCH_POINTS_PER_OCT)
}

/// Long-term average spectrum of a buffer on the Match EQ's log-frequency grid, in dB
fn average_spectrum_db(buf: &AudioBuffer) -> Vec<f32> {
    let n = STFT_SIZE;
    let fft = Radix2Fft::new(n);
    let window: Vec<f32> = (0..n).map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / n as f32).cos())).collect();
    let frames = buf.num_frames();
    let mut power = vec![0.0f64; STFT_BINS];
    let mut count = 0usize;

    // Up to ~800 evenly spaced analysis frames keeps long tracks fast
    let max_frames = 800usize;
    let available = if frames > n { (frames - n) / n + 1 } else { 0 };
    let stride = (available / max_frames).max(1);
    let mut re = vec![0.0f32; n];
    let mut im = vec![0.0f32; n];
    let mut frame = 0;
    while frame < available {
        let start = frame * n;
        for i in 0..n {
            let (l, r) = buf.get_stereo_frame(start + i);
            re[i] = 0.5 * (l + r) * window[i];
        }
        im.fill(0.0);
        fft.forward(&mut re, &mut im);
        for k in 0..STFT_BINS {
            power[k] += (re[k] * re[k] + im[k] * im[k]) as f64;
        }
        count += 1;
        frame += stride;
    }
    let count = count.max(1) as f64;
    let sr = buf.sample_rate as f32;

    (0..match_point_count())
        .map(|p| {
            let f = match_point_freq(p);
            let half = 2.0f32.powf(0.5 / MATCH_POINTS_PER_OCT);
            let lo = ((f / half) * n as f32 / sr).floor().max(1.0) as usize;
            let hi = (((f * half) * n as f32 / sr).ceil() as usize).max(lo).min(STFT_BINS - 1);
            if lo >= STFT_BINS {
                return -120.0;
            }
            let sum: f64 = power[lo..=hi].iter().sum();
            let avg = sum / ((hi - lo + 1) as f64 * count);
            (10.0 * (avg.max(1e-20)).log10()) as f32
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct MatchEq {
    pub enabled: bool,
    pub amount: f32,
    pub smoothing: usize,
    pub max_gain_db: f32,
    source_db: Vec<f32>,
    reference_db: Vec<f32>,
    curve_db: Vec<f32>,
    bin_gain: Vec<f32>,
    stft: StftProcessor,
    sample_rate: f32,
}

impl MatchEq {
    pub fn new(sample_rate: f32) -> Self {
        let mut m = Self {
            enabled: false,
            amount: 0.75,
            smoothing: 2,
            max_gain_db: 9.0,
            source_db: Vec::new(),
            reference_db: Vec::new(),
            curve_db: Vec::new(),
            bin_gain: vec![1.0; STFT_BINS],
            stft: StftProcessor::new(),
            sample_rate,
        };
        m.rebuild();
        m
    }

    pub fn is_learned(&self) -> bool {
        !self.curve_db.is_empty()
    }

    /// Capture the tonal difference between the track and a reference
    pub fn learn(&mut self, source: &AudioBuffer, reference: &AudioBuffer) -> Result<(), String> {
        if source.num_frames() < STFT_SIZE * 4 || reference.num_frames() < STFT_SIZE * 4 {
            return Err("Both tracks need at least a fraction of a second of audio".to_string());
        }
        self.source_db = average_spectrum_db(source);
        self.reference_db = average_spectrum_db(reference);
        self.rebuild();
        Ok(())
    }

    /// Correction curve on the log-frequency grid (empty until learned)
    pub fn curve_db(&self) -> &[f32] {
        &self.curve_db
    }

    fn rebuild(&mut self) {
        self.curve_db.clear();
        if self.source_db.len() == match_point_count() && self.reference_db.len() == self.source_db.len() {
            let width = (MATCH_SMOOTHING_OCT[self.smoothing.min(4)] * MATCH_POINTS_PER_OCT * 0.5).round() as isize;
            let smooth = |v: &[f32]| -> Vec<f32> {
                (0..v.len() as isize)
                    .map(|i| {
                        let lo = (i - width).max(0) as usize;
                        let hi = ((i + width) as usize).min(v.len() - 1);
                        v[lo..=hi].iter().sum::<f32>() / (hi - lo + 1) as f32
                    })
                    .collect()
            };
            let src = smooth(&self.source_db);
            let refr = smooth(&self.reference_db);
            let mut diff: Vec<f32> = refr.iter().zip(src.iter()).map(|(r, s)| r - s).collect();

            // Match tone, not level: remove the average offset across 100 Hz - 10 kHz
            let (sum, n) = diff
                .iter()
                .enumerate()
                .filter(|(p, _)| (100.0..=10000.0).contains(&match_point_freq(*p)))
                .fold((0.0, 0), |(s, n), (_, d)| (s + d, n + 1));
            let offset = if n > 0 { sum / n as f32 } else { 0.0 };
            for d in diff.iter_mut() {
                *d = (*d - offset).clamp(-self.max_gain_db, self.max_gain_db);
            }
            self.curve_db = diff;
        }

        for k in 0..STFT_BINS {
            let db = if self.curve_db.is_empty() {
                0.0
            } else {
                let f = bin_freq(k, self.sample_rate).clamp(MATCH_F_MIN, MATCH_F_MAX);
                let pos = (f / MATCH_F_MIN).log2() * MATCH_POINTS_PER_OCT;
                let i0 = (pos.floor() as usize).min(self.curve_db.len() - 1);
                let i1 = (i0 + 1).min(self.curve_db.len() - 1);
                let t = pos - i0 as f32;
                self.curve_db[i0] * (1.0 - t) + self.curve_db[i1] * t
            };
            self.bin_gain[k] = db_to_gain(db * self.amount);
        }
    }

    /// Learned spectra for preset storage: (source, reference)
    pub fn learned_spectra(&self) -> (&[f32], &[f32]) {
        (&self.source_db, &self.reference_db)
    }

    pub fn set_learned_spectra(&mut self, source: Vec<f32>, reference: Vec<f32>) {
        if source.len() == match_point_count() && reference.len() == source.len() {
            self.source_db = source;
            self.reference_db = reference;
            self.rebuild();
        }
    }

    pub fn learned_point_count() -> usize {
        match_point_count()
    }
}

impl Module for MatchEq {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, enabled: bool) {
        if enabled != self.enabled { self.stft.reset(); }
        self.enabled = enabled;
    }
    fn params(&self) -> Vec<ParamInfo> {
        vec![
            ParamInfo::percent("Amount", 0.0, 1.0),
            ParamInfo::choice("Smoothing", MATCH_SMOOTHING),
            ParamInfo::db("Max correction", 1.0, 18.0),
        ]
    }
    fn get_param(&self, i: usize) -> f32 {
        match i { 0 => self.amount, 1 => self.smoothing as f32, _ => self.max_gain_db }
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.amount = v.clamp(0.0, 1.0),
            1 => self.smoothing = (v.round().max(0.0) as usize).min(4),
            _ => self.max_gain_db = v.clamp(1.0, 18.0),
        }
        self.rebuild();
    }
    fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        self.rebuild();
    }
    fn reset(&mut self) { self.stft.reset(); }
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let gains = &self.bin_gain;
        self.stft.process(left, right, &mut |frame: &mut SpectralFrame| {
            for (k, &g) in gains.iter().enumerate() {
                frame.scale_bin(k, g);
            }
        });
    }
    fn latency(&self) -> usize { self.stft.latency() }
    fn status(&self) -> Option<String> {
        Some(if self.is_learned() {
            let max = self.curve_db.iter().fold(0.0f32, |a, d| a.max(d.abs()));
            format!("Learned curve (up to ±{:.1} dB)", max)
        } else {
            "Not learned: load a reference track, then click Learn".to_string()
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Spectral Shaper: tames narrow spectral peaks relative to the local spectral average
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct SpectralShaper {
    pub enabled: bool,
    pub low_hz: f32,
    pub high_hz: f32,
    pub threshold_db: f32,
    pub amount_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    reduction_db: Vec<f32>,
    mag_db: Vec<f32>,
    prefix: Vec<f32>,
    max_reduction: f32,
    stft: StftProcessor,
    sample_rate: f32,
}

impl SpectralShaper {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            enabled: false,
            low_hz: 2000.0,
            high_hz: 16000.0,
            threshold_db: 6.0,
            amount_db: 4.0,
            attack_ms: 10.0,
            release_ms: 120.0,
            reduction_db: vec![0.0; STFT_BINS],
            mag_db: vec![0.0; STFT_BINS],
            prefix: vec![0.0; STFT_BINS + 1],
            max_reduction: 0.0,
            stft: StftProcessor::new(),
            sample_rate,
        }
    }
}

impl Module for SpectralShaper {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, enabled: bool) {
        if enabled != self.enabled { self.reset(); }
        self.enabled = enabled;
    }
    fn params(&self) -> Vec<ParamInfo> {
        vec![
            ParamInfo::hz("Low limit", 200.0, 18000.0),
            ParamInfo::hz("High limit", 1000.0, 20000.0),
            ParamInfo::db("Threshold above average", 0.0, 24.0),
            ParamInfo::db("Max reduction", 0.0, 12.0),
            ParamInfo::new("Attack", ParamUnit::Ms, 1.0, 200.0, 2.0),
            ParamInfo::new("Release", ParamUnit::Ms, 10.0, 1000.0, 10.0),
        ]
    }
    fn get_param(&self, i: usize) -> f32 {
        [self.low_hz, self.high_hz, self.threshold_db, self.amount_db, self.attack_ms, self.release_ms][i.min(5)]
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.low_hz = v.clamp(200.0, self.high_hz),
            1 => self.high_hz = v.clamp(self.low_hz, 20000.0),
            2 => self.threshold_db = v.clamp(0.0, 24.0),
            3 => self.amount_db = v.clamp(0.0, 12.0),
            4 => self.attack_ms = v.clamp(1.0, 200.0),
            _ => self.release_ms = v.clamp(10.0, 1000.0),
        }
    }
    fn set_sample_rate(&mut self, sr: f32) { self.sample_rate = sr; }
    fn reset(&mut self) {
        self.stft.reset();
        self.reduction_db.fill(0.0);
    }
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let att = frame_coeff(self.attack_ms, sr);
        let rel = frame_coeff(self.release_ms, sr);
        let (low, high, thr, amt) = (self.low_hz, self.high_hz, self.threshold_db, self.amount_db);
        let reduction = &mut self.reduction_db;
        let mag_db = &mut self.mag_db;
        let prefix = &mut self.prefix;
        let mut frame_max = 0.0f32;

        self.stft.process(left, right, &mut |frame: &mut SpectralFrame| {
            for k in 0..STFT_BINS {
                mag_db[k] = gain_to_db(frame.mag_l(k).max(frame.mag_r(k)) + 1e-9);
                prefix[k + 1] = prefix[k] + mag_db[k];
            }
            for k in 1..STFT_BINS {
                let f = bin_freq(k, sr);
                // Band weight with half-octave soft edges
                let w = smoothstep(low / 1.41, low, f) * (1.0 - smoothstep(high, high * 1.41, f));
                let target = if w > 0.0 {
                    let half = (k / 10).max(3);
                    let lo = k.saturating_sub(half);
                    let hi = (k + half).min(STFT_BINS - 1);
                    let avg = (prefix[hi + 1] - prefix[lo]) / (hi - lo + 1) as f32;
                    ((mag_db[k] - avg - thr).max(0.0) * 0.7).min(amt) * w
                } else {
                    0.0
                };
                let c = if target > reduction[k] { att } else { rel };
                reduction[k] = target + c * (reduction[k] - target);
                frame_max = frame_max.max(reduction[k]);
                frame.scale_bin(k, db_to_gain(-reduction[k]));
            }
        });
        self.max_reduction = frame_max.max(self.max_reduction * 0.9);
    }
    fn latency(&self) -> usize { self.stft.latency() }
    fn status(&self) -> Option<String> {
        Some(format!("Peak reduction: {:.1} dB", self.max_reduction))
    }
    fn meters(&self) -> [f32; 4] {
        [self.max_reduction, 0.0, 0.0, 0.0]
    }
    fn set_meters(&mut self, m: [f32; 4]) {
        self.max_reduction = m[0];
    }
}

// ---------------------------------------------------------------------------------------------
// Tonal Stabilizer: slowly steers the long-term tonal balance toward a target tilt
// ---------------------------------------------------------------------------------------------

const STAB_BANDS: usize = 20;
const STAB_TARGETS: &[&str] = &["Flat (pink)", "Warm", "Balanced", "Bright"];
const STAB_TILT_DB_PER_OCT: [f32; 4] = [0.0, -1.0, -0.5, 0.5];

#[derive(Clone, Debug)]
pub struct TonalStabilizer {
    pub enabled: bool,
    pub amount: f32,
    pub speed_ms: f32,
    pub target: usize,
    pub max_correction_db: f32,
    band_level: [f32; STAB_BANDS],
    correction_db: [f32; STAB_BANDS],
    primed: bool,
    stft: StftProcessor,
    sample_rate: f32,
}

fn stab_band_center(b: usize) -> f32 {
    // 30 Hz .. 16 kHz, log spaced
    30.0 * (16000.0f32 / 30.0).powf((b as f32 + 0.5) / STAB_BANDS as f32)
}

fn stab_band_edge(b: usize) -> f32 {
    30.0 * (16000.0f32 / 30.0).powf(b as f32 / STAB_BANDS as f32)
}

impl TonalStabilizer {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            enabled: false,
            amount: 0.5,
            speed_ms: 2000.0,
            target: 2,
            max_correction_db: 6.0,
            band_level: [0.0; STAB_BANDS],
            correction_db: [0.0; STAB_BANDS],
            primed: false,
            stft: StftProcessor::new(),
            sample_rate,
        }
    }

    pub fn correction_db(&self) -> &[f32; STAB_BANDS] {
        &self.correction_db
    }
}

impl Module for TonalStabilizer {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, enabled: bool) {
        if enabled != self.enabled { self.reset(); }
        self.enabled = enabled;
    }
    fn params(&self) -> Vec<ParamInfo> {
        vec![
            ParamInfo::percent("Amount", 0.0, 1.0),
            ParamInfo::new("Speed", ParamUnit::Ms, 200.0, 10000.0, 100.0),
            ParamInfo::choice("Target", STAB_TARGETS),
            ParamInfo::db("Max correction", 1.0, 12.0),
        ]
    }
    fn get_param(&self, i: usize) -> f32 {
        match i { 0 => self.amount, 1 => self.speed_ms, 2 => self.target as f32, _ => self.max_correction_db }
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.amount = v.clamp(0.0, 1.0),
            1 => self.speed_ms = v.clamp(200.0, 10000.0),
            2 => self.target = (v.round().max(0.0) as usize).min(STAB_TARGETS.len() - 1),
            _ => self.max_correction_db = v.clamp(1.0, 12.0),
        }
    }
    fn set_sample_rate(&mut self, sr: f32) { self.sample_rate = sr; }
    fn reset(&mut self) {
        self.stft.reset();
        self.band_level = [0.0; STAB_BANDS];
        self.correction_db = [0.0; STAB_BANDS];
        self.primed = false;
    }
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let coeff = frame_coeff(self.speed_ms, sr);
        let tilt = STAB_TILT_DB_PER_OCT[self.target.min(3)];
        let (amount, max_corr) = (self.amount, self.max_correction_db);
        let band_level = &mut self.band_level;
        let correction = &mut self.correction_db;
        let primed = &mut self.primed;

        self.stft.process(left, right, &mut |frame: &mut SpectralFrame| {
            let mut power = [0.0f32; STAB_BANDS];
            let mut total = 0.0f32;
            for k in 1..STFT_BINS {
                let f = bin_freq(k, sr);
                if f < stab_band_edge(0) || f >= stab_band_edge(STAB_BANDS) {
                    continue;
                }
                let b = (((f / 30.0).ln() / (16000.0f32 / 30.0).ln()) * STAB_BANDS as f32) as usize;
                let p = frame.mag_l(k).powi(2) + frame.mag_r(k).powi(2);
                power[b.min(STAB_BANDS - 1)] += p;
                total += p;
            }
            // Don't learn from silence
            if total > 1e-6 {
                for b in 0..STAB_BANDS {
                    let db = 10.0 * (power[b] + 1e-12).log10();
                    band_level[b] = if *primed { db + coeff * (band_level[b] - db) } else { db };
                }
                *primed = true;
                // Bands are equal width in octaves, so pink noise reads flat; compare to the tilt
                let dev: Vec<f32> = (0..STAB_BANDS)
                    .map(|b| band_level[b] - tilt * (stab_band_center(b) / 1000.0).log2())
                    .collect();
                let mean = dev.iter().sum::<f32>() / STAB_BANDS as f32;
                for b in 0..STAB_BANDS {
                    correction[b] = (-(dev[b] - mean) * amount).clamp(-max_corr, max_corr);
                }
            }
            for k in 1..STFT_BINS {
                let f = bin_freq(k, sr).clamp(stab_band_center(0), stab_band_center(STAB_BANDS - 1));
                let pos = ((f / stab_band_center(0)).ln() / (stab_band_center(STAB_BANDS - 1) / stab_band_center(0)).ln())
                    * (STAB_BANDS - 1) as f32;
                let i0 = (pos.floor() as usize).min(STAB_BANDS - 1);
                let i1 = (i0 + 1).min(STAB_BANDS - 1);
                let t = pos - i0 as f32;
                frame.scale_bin(k, db_to_gain(correction[i0] * (1.0 - t) + correction[i1] * t));
            }
        });
    }
    fn latency(&self) -> usize { self.stft.latency() }
    fn status(&self) -> Option<String> {
        let [lo, hi, _, _] = self.meters();
        Some(format!("Correction: {:+.1} .. {:+.1} dB", lo, hi))
    }
    fn meters(&self) -> [f32; 4] {
        let (lo, hi) = self.correction_db.iter().fold((0.0f32, 0.0f32), |(a, b), &c| (a.min(c), b.max(c)));
        [lo, hi, 0.0, 0.0]
    }
    fn set_meters(&mut self, m: [f32; 4]) {
        // The UI copy only needs the range: spread it over the bands so `meters` reproduces it
        self.correction_db = [0.0; STAB_BANDS];
        self.correction_db[0] = m[0];
        self.correction_db[1] = m[1];
    }
}

// ---------------------------------------------------------------------------------------------
// Real-time stem masking (vocals / bass / drums / other) for Stem Rebalance and Stem EQ
// ---------------------------------------------------------------------------------------------

pub const STEM_NAMES: [&str; 4] = ["Vocals", "Bass", "Drums", "Other"];
const HISTORY: usize = 9;
const FREQ_MEDIAN_HALF: usize = 8;

/// Splits each STFT frame into soft masks that sum to 1 per bin:
/// drums = percussive energy (frequency-median vs time-median, a causal harmonic/percussive
/// split), bass = harmonic energy below ~200 Hz, vocals = centred harmonic energy in the voice
/// range, other = the remainder
#[derive(Clone, Debug)]
pub struct StemMasker {
    history: Vec<[f32; STFT_BINS]>,
    pos: usize,
    pub masks: [Vec<f32>; 4],
    mono: Vec<f32>,
}

impl Default for StemMasker {
    fn default() -> Self {
        Self {
            history: vec![[0.0; STFT_BINS]; HISTORY],
            pos: 0,
            masks: [vec![0.0; STFT_BINS], vec![0.0; STFT_BINS], vec![0.0; STFT_BINS], vec![1.0; STFT_BINS]],
            mono: vec![0.0; STFT_BINS],
        }
    }
}

impl StemMasker {
    pub fn reset(&mut self) {
        for h in self.history.iter_mut() {
            h.fill(0.0);
        }
        self.pos = 0;
    }

    pub fn analyze(&mut self, frame: &SpectralFrame, sample_rate: f32) {
        for k in 0..STFT_BINS {
            let mr = 0.5 * (frame.re_l[k] + frame.re_r[k]);
            let mi = 0.5 * (frame.im_l[k] + frame.im_r[k]);
            self.mono[k] = (mr * mr + mi * mi).sqrt();
        }
        self.history[self.pos].copy_from_slice(&self.mono);
        self.pos = (self.pos + 1) % HISTORY;

        let mut tbuf = [0.0f32; HISTORY];
        let mut fbuf = [0.0f32; 2 * FREQ_MEDIAN_HALF + 1];
        for k in 0..STFT_BINS {
            for (j, h) in self.history.iter().enumerate() {
                tbuf[j] = h[k];
            }
            tbuf.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let harm = tbuf[HISTORY / 2];

            let lo = k.saturating_sub(FREQ_MEDIAN_HALF);
            let hi = (k + FREQ_MEDIAN_HALF).min(STFT_BINS - 1);
            let len = hi - lo + 1;
            fbuf[..len].copy_from_slice(&self.mono[lo..=hi]);
            let window = &mut fbuf[..len];
            window.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let perc = window[len / 2];

            let p2 = perc * perc;
            let h2 = harm * harm;
            let perc_mask = p2 / (p2 + h2 + 1e-12);
            let harm_mask = 1.0 - perc_mask;

            let f = bin_freq(k, sample_rate);
            let bass_w = 1.0 - smoothstep(120.0, 250.0, f);
            let voice_band = smoothstep(150.0, 250.0, f) * (1.0 - smoothstep(6000.0, 9000.0, f));

            let mid2 = {
                let mr = 0.5 * (frame.re_l[k] + frame.re_r[k]);
                let mi = 0.5 * (frame.im_l[k] + frame.im_r[k]);
                mr * mr + mi * mi
            };
            let side2 = {
                let sr_ = 0.5 * (frame.re_l[k] - frame.re_r[k]);
                let si = 0.5 * (frame.im_l[k] - frame.im_r[k]);
                sr_ * sr_ + si * si
            };
            let centred = mid2 / (mid2 + side2 + 1e-12);

            let bass = harm_mask * bass_w;
            let upper = harm_mask * (1.0 - bass_w);
            let vocals = upper * voice_band * centred * centred;
            self.masks[0][k] = vocals;
            self.masks[1][k] = bass;
            self.masks[2][k] = perc_mask;
            self.masks[3][k] = (upper - vocals).max(0.0);
        }
    }
}

#[derive(Clone, Debug)]
pub struct StemRebalance {
    pub enabled: bool,
    /// Gain in dB for vocals, bass, drums, other
    pub gains_db: [f32; 4],
    masker: StemMasker,
    stft: StftProcessor,
    sample_rate: f32,
}

impl StemRebalance {
    pub fn new(sample_rate: f32) -> Self {
        Self { enabled: false, gains_db: [0.0; 4], masker: StemMasker::default(), stft: StftProcessor::new(), sample_rate }
    }
}

impl Module for StemRebalance {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, enabled: bool) {
        if enabled != self.enabled { self.reset(); }
        self.enabled = enabled;
    }
    fn params(&self) -> Vec<ParamInfo> {
        STEM_NAMES.iter().map(|n| ParamInfo::db(format!("{} level", n), -12.0, 12.0)).collect()
    }
    fn get_param(&self, i: usize) -> f32 { self.gains_db[i.min(3)] }
    fn set_param(&mut self, i: usize, v: f32) { self.gains_db[i.min(3)] = v.clamp(-12.0, 12.0); }
    fn set_sample_rate(&mut self, sr: f32) { self.sample_rate = sr; }
    fn reset(&mut self) {
        self.stft.reset();
        self.masker.reset();
    }
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let g = self.gains_db.map(db_to_gain);
        let masker = &mut self.masker;
        self.stft.process(left, right, &mut |frame: &mut SpectralFrame| {
            masker.analyze(frame, sr);
            for k in 0..STFT_BINS {
                let gain = (0..4).map(|s| masker.masks[s][k] * g[s]).sum::<f32>();
                frame.scale_bin(k, gain);
            }
        });
    }
    fn latency(&self) -> usize { self.stft.latency() }
}

#[derive(Clone, Debug)]
pub struct StemEq {
    pub enabled: bool,
    /// Per stem (vocals, bass, drums, other): low shelf, mid bell, high shelf gains in dB
    pub bands_db: [[f32; 3]; 4],
    gain_table: Vec<[f32; 4]>,
    masker: StemMasker,
    stft: StftProcessor,
    sample_rate: f32,
}

const STEM_EQ_BANDS: [&str; 3] = ["low", "mid", "high"];

impl StemEq {
    pub fn new(sample_rate: f32) -> Self {
        let mut eq = Self {
            enabled: false,
            bands_db: [[0.0; 3]; 4],
            gain_table: vec![[1.0; 4]; STFT_BINS],
            masker: StemMasker::default(),
            stft: StftProcessor::new(),
            sample_rate,
        };
        eq.rebuild();
        eq
    }

    fn rebuild(&mut self) {
        for k in 0..STFT_BINS {
            let f = bin_freq(k, self.sample_rate).max(10.0);
            let low_w = 1.0 / (1.0 + (f / 250.0).powi(2));
            let high_w = 1.0 / (1.0 + (5000.0 / f).powi(2));
            let mid_w = (-(f / 1500.0).log2().powi(2) / 2.0).exp();
            for s in 0..4 {
                let b = self.bands_db[s];
                self.gain_table[k][s] = db_to_gain(b[0] * low_w + b[1] * mid_w + b[2] * high_w);
            }
        }
    }
}

impl Module for StemEq {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, enabled: bool) {
        if enabled != self.enabled { self.reset(); }
        self.enabled = enabled;
    }
    fn params(&self) -> Vec<ParamInfo> {
        let mut p = Vec::new();
        for stem in STEM_NAMES {
            for band in STEM_EQ_BANDS {
                p.push(ParamInfo::db(format!("{} {}", stem, band), -12.0, 12.0));
            }
        }
        p
    }
    fn get_param(&self, i: usize) -> f32 { self.bands_db[(i / 3).min(3)][i % 3] }
    fn set_param(&mut self, i: usize, v: f32) {
        self.bands_db[(i / 3).min(3)][i % 3] = v.clamp(-12.0, 12.0);
        self.rebuild();
    }
    fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        self.rebuild();
    }
    fn reset(&mut self) {
        self.stft.reset();
        self.masker.reset();
    }
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let table = &self.gain_table;
        let masker = &mut self.masker;
        self.stft.process(left, right, &mut |frame: &mut SpectralFrame| {
            masker.analyze(frame, sr);
            for k in 0..STFT_BINS {
                let gain = (0..4).map(|s| masker.masks[s][k] * table[k][s]).sum::<f32>();
                frame.scale_bin(k, gain);
            }
        });
    }
    fn latency(&self) -> usize { self.stft.latency() }
}
