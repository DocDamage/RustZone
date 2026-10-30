use std::f32::consts::PI;

/// 2nd-order Butterworth biquad filter for building 4th-order Linkwitz-Riley crossovers
#[derive(Clone, Copy, Debug)]
pub(crate) struct Biquad {
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

impl Biquad {
    fn new_lowpass(fc: f32, fs: f32) -> Self {
        let w0 = 2.0 * PI * fc / fs;
        let cos_w = w0.cos();
        let sin_w = w0.sin();
        let alpha = sin_w / (2.0 * 0.70710678); // Butterworth Q = 1/sqrt(2)

        let b0 = (1.0 - cos_w) * 0.5;
        let b1 = 1.0 - cos_w;
        let b2 = (1.0 - cos_w) * 0.5;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w;
        let a2 = 1.0 - alpha;

        let inv = 1.0 / a0;
        Self {
            b0: b0 * inv,
            b1: b1 * inv,
            b2: b2 * inv,
            a1: a1 * inv,
            a2: a2 * inv,
            z1_l: 0.0,
            z2_l: 0.0,
            z1_r: 0.0,
            z2_r: 0.0,
        }
    }

    fn new_highpass(fc: f32, fs: f32) -> Self {
        let w0 = 2.0 * PI * fc / fs;
        let cos_w = w0.cos();
        let sin_w = w0.sin();
        let alpha = sin_w / (2.0 * 0.70710678);

        let b0 = (1.0 + cos_w) * 0.5;
        let b1 = -(1.0 + cos_w);
        let b2 = (1.0 + cos_w) * 0.5;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w;
        let a2 = 1.0 - alpha;

        let inv = 1.0 / a0;
        Self {
            b0: b0 * inv,
            b1: b1 * inv,
            b2: b2 * inv,
            a1: a1 * inv,
            a2: a2 * inv,
            z1_l: 0.0,
            z2_l: 0.0,
            z1_r: 0.0,
            z2_r: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let out_l = self.b0 * l + self.z1_l;
        self.z1_l = self.b1 * l - self.a1 * out_l + self.z2_l;
        self.z2_l = self.b2 * l - self.a2 * out_l;

        let out_r = self.b0 * r + self.z1_r;
        self.z1_r = self.b1 * r - self.a1 * out_r + self.z2_r;
        self.z2_r = self.b2 * r - self.a2 * out_r;

        (out_l, out_r)
    }

    fn reset(&mut self) {
        self.z1_l = 0.0;
        self.z2_l = 0.0;
        self.z1_r = 0.0;
        self.z2_r = 0.0;
    }
}

/// 4th-Order Linkwitz-Riley Crossover (cascaded Butterworth pair)
#[derive(Clone, Copy, Debug)]
pub(crate) struct LinkwitzRiley4 {
    lp1: Biquad,
    lp2: Biquad,
    hp1: Biquad,
    hp2: Biquad,
}

impl LinkwitzRiley4 {
    pub(crate) fn new(fc: f32, fs: f32) -> Self {
        Self {
            lp1: Biquad::new_lowpass(fc, fs),
            lp2: Biquad::new_lowpass(fc, fs),
            hp1: Biquad::new_highpass(fc, fs),
            hp2: Biquad::new_highpass(fc, fs),
        }
    }

    #[inline]
    pub(crate) fn split(&mut self, l: f32, r: f32) -> ((f32, f32), (f32, f32)) {
        let (l1, r1) = self.lp1.process(l, r);
        let low = self.lp2.process(l1, r1);

        let (h1, r_h1) = self.hp1.process(l, r);
        let high = self.hp2.process(h1, r_h1);

        (low, high)
    }

    pub(crate) fn reset(&mut self) {
        self.lp1.reset();
        self.lp2.reset();
        self.hp1.reset();
        self.hp2.reset();
    }
}

/// Mastering Dynamic Compressor / Expander Band
#[derive(Clone, Debug)]
pub struct DynamicsBand {
    pub enabled: bool,
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_gain_db: f32,
    pub gain_reduction_db: f32,
    envelope: f32,
}

impl DynamicsBand {
    pub fn new(threshold_db: f32, ratio: f32, attack_ms: f32, release_ms: f32, makeup_gain_db: f32) -> Self {
        Self {
            enabled: true,
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            makeup_gain_db,
            gain_reduction_db: 0.0,
            envelope: 0.0,
        }
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32, sample_rate: f32) -> (f32, f32) {
        if !self.enabled {
            return (l, r);
        }

        let peak = l.abs().max(r.abs());
        let att_coeff = (-1.0 / (self.attack_ms.max(0.1) * 0.001 * sample_rate)).exp();
        let rel_coeff = (-1.0 / (self.release_ms.max(1.0) * 0.001 * sample_rate)).exp();

        if peak > self.envelope {
            self.envelope = peak + att_coeff * (self.envelope - peak);
        } else {
            self.envelope = peak + rel_coeff * (self.envelope - peak);
        }

        let env_db = (20.0 * self.envelope.max(1e-5).log10()).clamp(-90.0, 10.0);
        let mut gain_db = 0.0f32;

        if env_db > self.threshold_db {
            let over_db = env_db - self.threshold_db;
            gain_db = -over_db * (1.0 - 1.0 / self.ratio.max(1.0));
        }

        self.gain_reduction_db = gain_db;
        let total_gain = 10.0f32.powf((gain_db + self.makeup_gain_db) / 20.0);

        (l * total_gain, r * total_gain)
    }

    pub fn reset(&mut self) {
        self.envelope = 0.0;
        self.gain_reduction_db = 0.0;
    }
}

/// 4-Band Multiband Mastering Dynamics Processor
#[derive(Clone, Debug)]
pub struct MultibandDynamics {
    pub enabled: bool,
    sample_rate: f32,
    crossover_low: LinkwitzRiley4,
    crossover_mid: LinkwitzRiley4,
    crossover_high: LinkwitzRiley4,
    pub bands: [DynamicsBand; 4],
}

impl MultibandDynamics {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(8000.0);
        Self {
            enabled: true,
            sample_rate: sr,
            crossover_low: LinkwitzRiley4::new(140.0, sr),
            crossover_mid: LinkwitzRiley4::new(1500.0, sr),
            crossover_high: LinkwitzRiley4::new(6000.0, sr),
            bands: [
                // Low Band (20 - 140 Hz)
                DynamicsBand::new(-16.0, 2.0, 25.0, 120.0, 0.4),
                // Low-Mid Band (140 - 1500 Hz)
                DynamicsBand::new(-18.0, 1.8, 20.0, 100.0, 0.2),
                // High-Mid Band (1500 - 6000 Hz)
                DynamicsBand::new(-19.0, 1.6, 15.0, 80.0, 0.0),
                // High Band (6000 - 20000 Hz)
                DynamicsBand::new(-20.0, 1.5, 10.0, 60.0, 0.3),
            ],
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = sample_rate.max(8000.0);
        self.sample_rate = sr;
        self.crossover_low = LinkwitzRiley4::new(140.0, sr);
        self.crossover_mid = LinkwitzRiley4::new(1500.0, sr);
        self.crossover_high = LinkwitzRiley4::new(6000.0, sr);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.crossover_low.reset();
        self.crossover_mid.reset();
        self.crossover_high.reset();
        for b in &mut self.bands {
            b.reset();
        }
    }

    pub fn process_block(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.enabled {
            return;
        }

        let n = left.len().min(right.len());
        let sr = self.sample_rate;

        for i in 0..n {
            let in_l = left[i];
            let in_r = right[i];

            // 1. Split into Low (<140Hz) and MidHigh (>140Hz)
            let (b0_low, mid_high) = self.crossover_low.split(in_l, in_r);

            // 2. Split MidHigh into LowMid (<1.5kHz) and High (>1.5kHz)
            let (b1_low_mid, high_split) = self.crossover_mid.split(mid_high.0, mid_high.1);

            // 3. Split High into HighMid (<6kHz) and Air (>6kHz)
            let (b2_high_mid, b3_air) = self.crossover_high.split(high_split.0, high_split.1);

            // 4. Process each band independently
            let out_b0 = self.bands[0].process(b0_low.0, b0_low.1, sr);
            let out_b1 = self.bands[1].process(b1_low_mid.0, b1_low_mid.1, sr);
            let out_b2 = self.bands[2].process(b2_high_mid.0, b2_high_mid.1, sr);
            let out_b3 = self.bands[3].process(b3_air.0, b3_air.1, sr);

            // 5. Phase-coherent sum back into stereo master
            left[i] = out_b0.0 + out_b1.0 + out_b2.0 + out_b3.0;
            right[i] = out_b0.1 + out_b1.1 + out_b2.1 + out_b3.1;
        }
    }
}

