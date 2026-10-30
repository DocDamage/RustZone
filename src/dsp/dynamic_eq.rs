use std::f32::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EqFilterType {
    HighPass,
    LowShelf,
    Peaking,
    HighShelf,
    LowPass,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EqChannelMode {
    Stereo,
    MidOnly,
    SideOnly,
}

#[derive(Clone, Debug)]
pub struct EqBand {
    pub enabled: bool,
    pub filter_type: EqFilterType,
    pub freq: f32,
    pub q: f32,
    pub static_gain_db: f32,
    pub channel_mode: EqChannelMode,
    
    // Dynamic EQ parameters
    pub dynamic_enabled: bool,
    pub threshold_db: f32,
    pub dynamic_range_db: f32, // Maximum dynamic gain offset
    pub attack_ms: f32,
    pub release_ms: f32,
    
    // Internal biquad coefficients & state
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1_l: f32,
    z2_l: f32,
    z1_r: f32,
    z2_r: f32,
    
    // Dynamic envelope detector
    envelope: f32,
    sample_rate: f32,
}

impl EqBand {
    pub fn new(filter_type: EqFilterType, freq: f32, q: f32, gain_db: f32, sample_rate: f32) -> Self {
        let mut band = Self {
            enabled: true,
            filter_type,
            freq,
            q,
            static_gain_db: gain_db,
            channel_mode: EqChannelMode::Stereo,
            dynamic_enabled: false,
            threshold_db: -18.0,
            dynamic_range_db: -4.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1_l: 0.0,
            z2_l: 0.0,
            z1_r: 0.0,
            z2_r: 0.0,
            envelope: 0.0,
            sample_rate,
        };
        band.update_coefficients(gain_db);
        band
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.update_coefficients(self.static_gain_db);
    }

    pub fn set_params(&mut self, filter_type: EqFilterType, freq: f32, q: f32, gain_db: f32) {
        self.filter_type = filter_type;
        self.freq = freq;
        self.q = q;
        self.static_gain_db = gain_db;
        self.update_coefficients(gain_db);
    }

    pub fn reset_state(&mut self) {
        self.z1_l = 0.0;
        self.z2_l = 0.0;
        self.z1_r = 0.0;
        self.z2_r = 0.0;
        self.envelope = 0.0;
    }

    pub fn update_coefficients(&mut self, gain_db: f32) {
        let sr = self.sample_rate.max(8000.0);
        let f0 = self.freq.clamp(10.0, sr * 0.49);
        let w0 = 2.0 * PI * f0 / sr;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * self.q.max(0.1));
        let a = 10.0f32.powf(gain_db / 40.0); // sqrt(A)

        let (b0, b1, b2, a0, a1, a2) = match self.filter_type {
            EqFilterType::Peaking => {
                let b0 = 1.0 + alpha * a;
                let b1 = -2.0 * cos_w0;
                let b2 = 1.0 - alpha * a;
                let a0 = 1.0 + alpha / a;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha / a;
                (b0, b1, b2, a0, a1, a2)
            }
            EqFilterType::LowShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
                let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
                let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
                let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
                let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
                let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            EqFilterType::HighShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
                let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
                let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
                let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
                let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
                let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            EqFilterType::HighPass => {
                let b0 = (1.0 + cos_w0) * 0.5;
                let b1 = -(1.0 + cos_w0);
                let b2 = (1.0 + cos_w0) * 0.5;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            EqFilterType::LowPass => {
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
        if !self.enabled {
            return (in_l, in_r);
        }

        // Mid/Side matrixing based on channel mode
        let (ch_a, ch_b) = match self.channel_mode {
            EqChannelMode::Stereo => (in_l, in_r),
            EqChannelMode::MidOnly | EqChannelMode::SideOnly => {
                let mid = (in_l + in_r) * 0.5;
                let side = (in_l - in_r) * 0.5;
                (mid, side)
            }
        };

        // If dynamic mode is active, modulate filter gain
        if self.dynamic_enabled && self.filter_type == EqFilterType::Peaking {
            let input_peak = in_l.abs().max(in_r.abs());
            let att_coeff = (-1.0 / (self.attack_ms.max(0.1) * 0.001 * self.sample_rate)).exp();
            let rel_coeff = (-1.0 / (self.release_ms.max(1.0) * 0.001 * self.sample_rate)).exp();

            if input_peak > self.envelope {
                self.envelope = input_peak + att_coeff * (self.envelope - input_peak);
            } else {
                self.envelope = input_peak + rel_coeff * (self.envelope - input_peak);
            }

            let env_db = (20.0 * self.envelope.max(1e-5).log10()).clamp(-80.0, 10.0);
            if env_db > self.threshold_db {
                let over_db = env_db - self.threshold_db;
                let dynamic_offset = (over_db * 0.5).min(self.dynamic_range_db.abs()) * self.dynamic_range_db.signum();
                self.update_coefficients(self.static_gain_db + dynamic_offset);
            } else {
                self.update_coefficients(self.static_gain_db);
            }
        }

        // Apply Direct Form II Transposed filtering
        let (filtered_a, filtered_b) = match self.channel_mode {
            EqChannelMode::Stereo => {
                let out_l = self.b0 * ch_a + self.z1_l;
                self.z1_l = self.b1 * ch_a - self.a1 * out_l + self.z2_l;
                self.z2_l = self.b2 * ch_a - self.a2 * out_l;

                let out_r = self.b0 * ch_b + self.z1_r;
                self.z1_r = self.b1 * ch_b - self.a1 * out_r + self.z2_r;
                self.z2_r = self.b2 * ch_b - self.a2 * out_r;

                (out_l, out_r)
            }
            EqChannelMode::MidOnly => {
                // Filter Mid (ch_a), pass Side (ch_b) untouched
                let out_mid = self.b0 * ch_a + self.z1_l;
                self.z1_l = self.b1 * ch_a - self.a1 * out_mid + self.z2_l;
                self.z2_l = self.b2 * ch_a - self.a2 * out_mid;

                let out_l = out_mid + ch_b;
                let out_r = out_mid - ch_b;
                (out_l, out_r)
            }
            EqChannelMode::SideOnly => {
                // Pass Mid (ch_a) untouched, filter Side (ch_b)
                let out_side = self.b0 * ch_b + self.z1_r;
                self.z1_r = self.b1 * ch_b - self.a1 * out_side + self.z2_r;
                self.z2_r = self.b2 * ch_b - self.a2 * out_side;

                let out_l = ch_a + out_side;
                let out_r = ch_a - out_side;
                (out_l, out_r)
            }
        };

        (filtered_a, filtered_b)
    }
}

/// 8-Band Mastering Parametric & Dynamic Equalizer
#[derive(Clone, Debug)]
pub struct DynamicEqualizer {
    pub enabled: bool,
    pub bands: [EqBand; 8],
}

impl DynamicEqualizer {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            enabled: true,
            bands: [
                EqBand::new(EqFilterType::HighPass, 25.0, 0.707, 0.0, sample_rate),
                EqBand::new(EqFilterType::LowShelf, 75.0, 0.707, 1.2, sample_rate),
                EqBand::new(EqFilterType::Peaking, 180.0, 1.2, -0.6, sample_rate),
                EqBand::new(EqFilterType::Peaking, 650.0, 1.0, 0.0, sample_rate),
                EqBand::new(EqFilterType::Peaking, 1800.0, 1.1, -0.4, sample_rate),
                EqBand::new(EqFilterType::Peaking, 4500.0, 1.2, 0.8, sample_rate),
                EqBand::new(EqFilterType::HighShelf, 11500.0, 0.707, 1.8, sample_rate),
                EqBand::new(EqFilterType::LowPass, 22000.0, 0.707, 0.0, sample_rate),
            ],
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        for band in &mut self.bands {
            band.set_sample_rate(sample_rate);
        }
    }

    pub fn reset_state(&mut self) {
        for band in &mut self.bands {
            band.reset_state();
        }
    }

    pub fn process_block(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.enabled {
            return;
        }
        let n = left.len().min(right.len());
        for i in 0..n {
            let mut cur_l = left[i];
            let mut cur_r = right[i];
            for band in &mut self.bands {
                let (next_l, next_r) = band.process_sample(cur_l, cur_r);
                cur_l = next_l;
                cur_r = next_r;
            }
            left[i] = cur_l;
            right[i] = cur_r;
        }
    }
}
