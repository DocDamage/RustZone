use std::f32::consts::PI;

/// Spectral Clarity & Dynamic Resonance Tamer
#[derive(Clone, Debug)]
pub struct SpectralClarity {
    pub enabled: bool,
    pub amount: f32, // 0.0 to 1.0 (subtle to intense resonance smoothing)
    pub focus_freq: f32,
    sample_rate: f32,
    
    // Split filter
    hp_b0: f32,
    hp_b1: f32,
    hp_b2: f32,
    hp_a1: f32,
    hp_a2: f32,
    z1_l: f32,
    z2_l: f32,
    z1_r: f32,
    z2_r: f32,

    // Envelope
    envelope: f32,
}

impl SpectralClarity {
    pub fn new(sample_rate: f32) -> Self {
        let mut sc = Self {
            enabled: true,
            amount: 0.25,
            focus_freq: 3500.0,
            sample_rate: sample_rate.max(8000.0),
            hp_b0: 1.0,
            hp_b1: 0.0,
            hp_b2: 0.0,
            hp_a1: 0.0,
            hp_a2: 0.0,
            z1_l: 0.0,
            z2_l: 0.0,
            z1_r: 0.0,
            z2_r: 0.0,
            envelope: 0.0,
        };
        sc.update_filter();
        sc
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(8000.0);
        self.update_filter();
        self.reset();
    }

    pub fn set_focus_freq(&mut self, freq: f32) {
        self.focus_freq = freq.clamp(500.0, self.sample_rate * 0.45);
        self.update_filter();
    }

    pub fn reset(&mut self) {
        self.z1_l = 0.0;
        self.z2_l = 0.0;
        self.z1_r = 0.0;
        self.z2_r = 0.0;
        self.envelope = 0.0;
    }

    fn update_filter(&mut self) {
        let sr = self.sample_rate;
        let w0 = 2.0 * PI * self.focus_freq / sr;
        let cos_w = w0.cos();
        let sin_w = w0.sin();
        let alpha = sin_w / (2.0 * 0.707);

        let b0 = (1.0 + cos_w) * 0.5;
        let b1 = -(1.0 + cos_w);
        let b2 = (1.0 + cos_w) * 0.5;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w;
        let a2 = 1.0 - alpha;

        let inv = 1.0 / a0;
        self.hp_b0 = b0 * inv;
        self.hp_b1 = b1 * inv;
        self.hp_b2 = b2 * inv;
        self.hp_a1 = a1 * inv;
        self.hp_a2 = a2 * inv;
    }

    pub fn process_block(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.enabled || self.amount <= 0.001 {
            return;
        }

        let n = left.len().min(right.len());
        let att_coeff = (-1.0 / (2.0 * 0.001 * self.sample_rate)).exp();
        let rel_coeff = (-1.0 / (30.0 * 0.001 * self.sample_rate)).exp();

        for i in 0..n {
            let in_l = left[i];
            let in_r = right[i];

            // Extract high-frequency resonance band
            let hp_l = self.hp_b0 * in_l + self.z1_l;
            self.z1_l = self.hp_b1 * in_l - self.hp_a1 * hp_l + self.z2_l;
            self.z2_l = self.hp_b2 * in_l - self.hp_a2 * hp_l;

            let hp_r = self.hp_b0 * in_r + self.z1_r;
            self.z1_r = self.hp_b1 * in_r - self.hp_a1 * hp_r + self.z2_r;
            self.z2_r = self.hp_b2 * in_r - self.hp_a2 * hp_r;

            // Detect harsh high-frequency energy
            let peak = hp_l.abs().max(hp_r.abs());
            if peak > self.envelope {
                self.envelope = peak + att_coeff * (self.envelope - peak);
            } else {
                self.envelope = peak + rel_coeff * (self.envelope - peak);
            }

            // Dynamic resonance smoothing
            let compression = (self.envelope * self.amount * 1.5).min(0.6);
            let smooth_factor = 1.0 - compression;

            left[i] = (in_l - hp_l) + hp_l * smooth_factor;
            right[i] = (in_r - hp_r) + hp_r * smooth_factor;
        }
    }
}
