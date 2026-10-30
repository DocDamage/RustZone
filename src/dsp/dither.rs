#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DitherType {
    None,
    Tpdf,
    NoiseShaped,
}

/// Standalone Audiophile Mastering Dither (TPDF & Psychoacoustic Noise Shaping)
#[derive(Clone, Debug)]
pub struct MasteringDither {
    pub dither_type: DitherType,
    pub target_bits: u32, // 16 or 24
    rng_state: u64,
    // Error feedback history for psychoacoustic noise shaping (Lipshitz / E-weighted)
    error_l1: f32,
    error_l2: f32,
    error_r1: f32,
    error_r2: f32,
}

impl MasteringDither {
    pub fn new(dither_type: DitherType, target_bits: u32) -> Self {
        Self {
            dither_type,
            target_bits: target_bits.clamp(16, 24),
            rng_state: 0x853C49E6748FEA9B,
            error_l1: 0.0,
            error_l2: 0.0,
            error_r1: 0.0,
            error_r2: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.error_l1 = 0.0;
        self.error_l2 = 0.0;
        self.error_r1 = 0.0;
        self.error_r2 = 0.0;
    }

    /// Linear congruential PRNG for high-speed uniform random floats in [-1.0, 1.0]
    #[inline]
    fn rand_uniform(&mut self) -> f32 {
        self.rng_state = self.rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let val = (self.rng_state >> 33) as f32 / 2147483648.0;
        val * 2.0 - 1.0
    }

    /// Process a stereo frame and apply dither & quantization noise shaping
    #[inline]
    pub fn process_sample(&mut self, l: f32, r: f32) -> (f32, f32) {
        if self.dither_type == DitherType::None {
            return (l, r);
        }

        let q_step = 1.0 / (2.0f32.powi(self.target_bits as i32 - 1));

        // Generate TPDF noise: sum of two independent uniform random variables in [-0.5, 0.5]
        let r1_l = self.rand_uniform() * 0.5;
        let r2_l = self.rand_uniform() * 0.5;
        let tpdf_l = (r1_l + r2_l) * q_step;

        let r1_r = self.rand_uniform() * 0.5;
        let r2_r = self.rand_uniform() * 0.5;
        let tpdf_r = (r1_r + r2_r) * q_step;

        match self.dither_type {
            DitherType::Tpdf => {
                let quant_l = ((l + tpdf_l) / q_step).round() * q_step;
                let quant_r = ((r + tpdf_r) / q_step).round() * q_step;
                (quant_l.clamp(-1.0, 1.0), quant_r.clamp(-1.0, 1.0))
            }
            DitherType::NoiseShaped => {
                // F-weighted psychoacoustic noise shaping feedback: H(z) = 1.62 z^-1 - 0.78 z^-2
                let shaped_l = l + tpdf_l + (1.62 * self.error_l1 - 0.78 * self.error_l2);
                let quant_l = (shaped_l / q_step).round() * q_step;
                self.error_l2 = self.error_l1;
                self.error_l1 = (shaped_l - quant_l).clamp(-2.0 * q_step, 2.0 * q_step);

                let shaped_r = r + tpdf_r + (1.62 * self.error_r1 - 0.78 * self.error_r2);
                let quant_r = (shaped_r / q_step).round() * q_step;
                self.error_r2 = self.error_r1;
                self.error_r1 = (shaped_r - quant_r).clamp(-2.0 * q_step, 2.0 * q_step);

                (quant_l.clamp(-1.0, 1.0), quant_r.clamp(-1.0, 1.0))
            }
            DitherType::None => (l, r),
        }
    }
}

