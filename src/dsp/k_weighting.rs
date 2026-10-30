use std::f32::consts::PI;

/// ITU-R BS.1770-4 K-weighting biquad filter coefficients
#[derive(Clone, Copy, Debug)]
pub struct BiquadCoeffs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl BiquadCoeffs {
    #[inline]
    pub fn process_sample(&self, x: f32, z1: &mut f32, z2: &mut f32) -> f32 {
        let y = self.b0 * x + *z1;
        *z1 = self.b1 * x - self.a1 * y + *z2;
        *z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Running K-weighted loudness over an exponential window, used for gain matching (A/B and
/// per-module auto gain) rather than for standards-compliant metering
#[derive(Clone, Debug)]
pub struct LoudnessFollower {
    filter: KWeightingFilter,
    mean_square: f64,
    window_s: f32,
    sample_rate: f32,
    primed: bool,
}

impl LoudnessFollower {
    pub fn new(sample_rate: f32, window_s: f32) -> Self {
        Self { filter: KWeightingFilter::new(sample_rate), mean_square: 0.0, window_s, sample_rate, primed: false }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.filter.set_sample_rate(sample_rate);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.filter.reset();
        self.mean_square = 0.0;
        self.primed = false;
    }

    pub fn feed(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len().min(right.len());
        if n == 0 {
            return;
        }
        let mut sum = 0.0f64;
        for i in 0..n {
            let (kl, kr) = self.filter.process_sample(left[i], right[i]);
            sum += (kl * kl + kr * kr) as f64;
        }
        let block_ms = sum / n as f64;
        if !self.primed {
            if block_ms > 1e-9 {
                self.mean_square = block_ms;
                self.primed = true;
            }
            return;
        }
        let c = (-(n as f64) / (self.window_s as f64 * self.sample_rate as f64)).exp();
        self.mean_square = self.mean_square * c + block_ms * (1.0 - c);
    }

    /// True once there is signal above roughly -70 LUFS
    pub fn is_active(&self) -> bool {
        self.primed && self.mean_square > 1e-7
    }

    pub fn lufs(&self) -> f32 {
        (-0.691 + 10.0 * self.mean_square.max(1e-12).log10()) as f32
    }
}

/// Dual-stage K-Weighting Filter (Stage 1: High Shelf, Stage 2: RLB High-Pass)
#[derive(Clone, Debug)]
pub struct KWeightingFilter {
    stage1_coeffs: BiquadCoeffs,
    stage2_coeffs: BiquadCoeffs,
    z1_s1_l: f32,
    z2_s1_l: f32,
    z1_s2_l: f32,
    z2_s2_l: f32,
    z1_s1_r: f32,
    z2_s1_r: f32,
    z1_s2_r: f32,
    z2_s2_r: f32,
}

impl KWeightingFilter {
    pub fn new(sample_rate: f32) -> Self {
        let (s1, s2) = Self::compute_coefficients(sample_rate);
        Self {
            stage1_coeffs: s1,
            stage2_coeffs: s2,
            z1_s1_l: 0.0,
            z2_s1_l: 0.0,
            z1_s2_l: 0.0,
            z2_s2_l: 0.0,
            z1_s1_r: 0.0,
            z2_s1_r: 0.0,
            z1_s2_r: 0.0,
            z2_s2_r: 0.0,
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        let (s1, s2) = Self::compute_coefficients(sample_rate);
        self.stage1_coeffs = s1;
        self.stage2_coeffs = s2;
        self.reset();
    }

    pub fn reset(&mut self) {
        self.z1_s1_l = 0.0;
        self.z2_s1_l = 0.0;
        self.z1_s2_l = 0.0;
        self.z2_s2_l = 0.0;
        self.z1_s1_r = 0.0;
        self.z2_s1_r = 0.0;
        self.z1_s2_r = 0.0;
        self.z2_s2_r = 0.0;
    }

    /// Process a stereo frame and return the K-weighted (left, right) samples
    #[inline]
    pub fn process_sample(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        // Stage 1 (High Shelf)
        let s1_l = self.stage1_coeffs.process_sample(in_l, &mut self.z1_s1_l, &mut self.z2_s1_l);
        let s1_r = self.stage1_coeffs.process_sample(in_r, &mut self.z1_s1_r, &mut self.z2_s1_r);

        // Stage 2 (RLB High Pass)
        let out_l = self.stage2_coeffs.process_sample(s1_l, &mut self.z1_s2_l, &mut self.z2_s2_l);
        let out_r = self.stage2_coeffs.process_sample(s1_r, &mut self.z1_s2_r, &mut self.z2_s2_r);

        (out_l, out_r)
    }

    fn compute_coefficients(fs: f32) -> (BiquadCoeffs, BiquadCoeffs) {
        let fs = fs.max(8000.0) as f64;

        // Stage 1: High shelf filter (+4 dB at 1681.97 Hz)
        let db_gain = 3.9998438f64;
        let f0 = 1681.974448f64;
        let q = 0.70717523695673f64;
        let k = (PI as f64 * f0 / fs).tan();
        let vh = 10.0f64.powf(db_gain / 20.0);
        let vb = vh.powf(0.499666774155);

        let a0 = 1.0 + k / q + k * k;
        let b0 = ((vh + vb * k / q + k * k) / a0) as f32;
        let b1 = (2.0 * (k * k - vh) / a0) as f32;
        let b2 = ((vh - vb * k / q + k * k) / a0) as f32;
        let a1 = (2.0 * (k * k - 1.0) / a0) as f32;
        let a2 = ((1.0 - k / q + k * k) / a0) as f32;

        let stage1 = BiquadCoeffs { b0, b1, b2, a1, a2 };

        // Stage 2: High pass (RLB) filter (f0 = 38.135 Hz)
        let f0_hp = 38.13547087602444f64;
        let q_hp = 0.50032703732539f64;
        let k_hp = (PI as f64 * f0_hp / fs).tan();
        let a0_hp = 1.0 + k_hp / q_hp + k_hp * k_hp;
        let b0_hp = (1.0 / a0_hp) as f32;
        let b1_hp = (-2.0 / a0_hp) as f32;
        let b2_hp = (1.0 / a0_hp) as f32;
        let a1_hp = (2.0 * (k_hp * k_hp - 1.0) / a0_hp) as f32;
        let a2_hp = ((1.0 - k_hp / q_hp + k_hp * k_hp) / a0_hp) as f32;

        let stage2 = BiquadCoeffs {
            b0: b0_hp,
            b1: b1_hp,
            b2: b2_hp,
            a1: a1_hp,
            a2: a2_hp,
        };

        (stage1, stage2)
    }
}

