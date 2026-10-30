use super::k_weighting::KWeightingFilter;

#[derive(Clone, Copy, Debug, Default)]
pub struct LoudnessReadout {
    pub momentary_lufs: f32,
    pub short_term_lufs: f32,
    pub integrated_lufs: f32,
    pub true_peak_l_dbtp: f32,
    pub true_peak_r_dbtp: f32,
    pub phase_correlation: f32,
}

/// ITU-R BS.1770-4 / EBU R128 Professional Mastering Loudness Meter
#[derive(Clone, Debug)]
pub struct EbuR128Meter {
    k_filter: KWeightingFilter,
    sample_rate: f32,
    block_size_100ms: usize,
    block_size_400ms: usize,
    block_size_3000ms: usize,
    
    // Circular buffer for 100ms sub-blocks
    sample_accumulator_l: f64,
    sample_accumulator_r: f64,
    sample_count: usize,

    // Gated blocks of power (mean square of K-weighted samples)
    sub_blocks_100ms: Vec<f64>,
    
    // Sliding buffers for short-term and momentary
    window_400ms: Vec<f64>,
    window_3000ms: Vec<f64>,

    // True Peak 4x oversampling state
    tp_history_l: [f32; 4],
    tp_history_r: [f32; 4],
    max_true_peak_l: f32,
    max_true_peak_r: f32,

    // Phase correlation state
    sum_l_r: f64,
    sum_l_sq: f64,
    sum_r_sq: f64,
    phase_count: usize,
    current_correlation: f32,
}

impl EbuR128Meter {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(8000.0);
        let block_100 = (sr * 0.1) as usize;
        let block_400 = (sr * 0.4) as usize;
        let block_3000 = (sr * 3.0) as usize;

        Self {
            k_filter: KWeightingFilter::new(sr),
            sample_rate: sr,
            block_size_100ms: block_100,
            block_size_400ms: block_400,
            block_size_3000ms: block_3000,
            sample_accumulator_l: 0.0,
            sample_accumulator_r: 0.0,
            sample_count: 0,
            sub_blocks_100ms: Vec::with_capacity(10000),
            window_400ms: Vec::with_capacity(4),
            window_3000ms: Vec::with_capacity(30),
            tp_history_l: [0.0; 4],
            tp_history_r: [0.0; 4],
            max_true_peak_l: 0.0,
            max_true_peak_r: 0.0,
            sum_l_r: 0.0,
            sum_l_sq: 0.0,
            sum_r_sq: 0.0,
            phase_count: 0,
            current_correlation: 1.0,
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = sample_rate.max(8000.0);
        self.sample_rate = sr;
        self.block_size_100ms = (sr * 0.1) as usize;
        self.block_size_400ms = (sr * 0.4) as usize;
        self.block_size_3000ms = (sr * 3.0) as usize;
        self.k_filter.set_sample_rate(sr);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.k_filter.reset();
        self.sample_accumulator_l = 0.0;
        self.sample_accumulator_r = 0.0;
        self.sample_count = 0;
        self.sub_blocks_100ms.clear();
        self.window_400ms.clear();
        self.window_3000ms.clear();
        self.tp_history_l = [0.0; 4];
        self.tp_history_r = [0.0; 4];
        self.max_true_peak_l = 0.0;
        self.max_true_peak_r = 0.0;
        self.sum_l_r = 0.0;
        self.sum_l_sq = 0.0;
        self.sum_r_sq = 0.0;
        self.phase_count = 0;
        self.current_correlation = 1.0;
    }

    /// Process a block of stereo samples
    pub fn process_block(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len().min(right.len());
        for i in 0..n {
            let s_l = left[i];
            let s_r = right[i];

            // 1. True Peak 4x Sinc Oversampling approximation
            self.update_true_peak(s_l, s_r);

            // 2. Phase Correlation accumulation
            self.sum_l_r += (s_l * s_r) as f64;
            self.sum_l_sq += (s_l * s_l) as f64;
            self.sum_r_sq += (s_r * s_r) as f64;
            self.phase_count += 1;
            if self.phase_count >= (self.sample_rate * 0.05) as usize { // update every 50ms
                let denom = (self.sum_l_sq * self.sum_r_sq).sqrt();
                if denom > 1e-9 {
                    self.current_correlation = (self.sum_l_r / denom).clamp(-1.0, 1.0) as f32;
                }
                self.sum_l_r = 0.0;
                self.sum_l_sq = 0.0;
                self.sum_r_sq = 0.0;
                self.phase_count = 0;
            }

            // 3. K-Weighting filtering
            let (k_l, k_r) = self.k_filter.process_sample(s_l, s_r);
            self.sample_accumulator_l += (k_l * k_l) as f64;
            self.sample_accumulator_r += (k_r * k_r) as f64;
            self.sample_count += 1;

            if self.sample_count >= self.block_size_100ms {
                let mean_l = self.sample_accumulator_l / self.sample_count as f64;
                let mean_r = self.sample_accumulator_r / self.sample_count as f64;
                let power_100ms = mean_l + mean_r;

                // Push into sliding windows
                self.window_400ms.push(power_100ms);
                if self.window_400ms.len() > 4 {
                    self.window_400ms.remove(0);
                }

                self.window_3000ms.push(power_100ms);
                if self.window_3000ms.len() > 30 {
                    self.window_3000ms.remove(0);
                }

                self.sub_blocks_100ms.push(power_100ms);

                self.sample_accumulator_l = 0.0;
                self.sample_accumulator_r = 0.0;
                self.sample_count = 0;
            }
        }
    }

    #[inline]
    fn update_true_peak(&mut self, l: f32, r: f32) {
        // Shift history
        self.tp_history_l[3] = self.tp_history_l[2];
        self.tp_history_l[2] = self.tp_history_l[1];
        self.tp_history_l[1] = self.tp_history_l[0];
        self.tp_history_l[0] = l;

        self.tp_history_r[3] = self.tp_history_r[2];
        self.tp_history_r[2] = self.tp_history_r[1];
        self.tp_history_r[1] = self.tp_history_r[0];
        self.tp_history_r[0] = r;

        // 4x Catmull-Rom / Sinc inter-sample peak evaluation
        let peak_interp = |hist: &[f32; 4]| -> f32 {
            let p0 = hist[3];
            let p1 = hist[2];
            let p2 = hist[1];
            let p3 = hist[0];

            let mut max_val = p1.abs().max(p2.abs());
            // Evaluate at t = 0.25, 0.5, 0.75
            for &t in &[0.25f32, 0.5f32, 0.75f32] {
                let t2 = t * t;
                let t3 = t2 * t;
                let v = 0.5 * (
                    (2.0 * p1) +
                    (-p0 + p2) * t +
                    (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2 +
                    (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3
                );
                let av = v.abs();
                if av > max_val {
                    max_val = av;
                }
            }
            max_val
        };

        let tp_l = peak_interp(&self.tp_history_l);
        let tp_r = peak_interp(&self.tp_history_r);

        if tp_l > self.max_true_peak_l {
            self.max_true_peak_l = tp_l;
        } else {
            self.max_true_peak_l *= 0.99995; // very slow decay for mastering meter display
        }

        if tp_r > self.max_true_peak_r {
            self.max_true_peak_r = tp_r;
        } else {
            self.max_true_peak_r *= 0.99995;
        }
    }

    /// Calculate the current Momentary Loudness (400ms window) in LUFS
    pub fn momentary_lufs(&self) -> f32 {
        if self.window_400ms.is_empty() {
            return -70.0;
        }
        let sum: f64 = self.window_400ms.iter().sum();
        let mean = sum / self.window_400ms.len() as f64;
        Self::power_to_lufs(mean)
    }

    /// Calculate the current Short-Term Loudness (3.0s window) in LUFS
    pub fn short_term_lufs(&self) -> f32 {
        if self.window_3000ms.is_empty() {
            return -70.0;
        }
        let sum: f64 = self.window_3000ms.iter().sum();
        let mean = sum / self.window_3000ms.len() as f64;
        Self::power_to_lufs(mean)
    }

    /// Calculate ITU-R BS.1770-4 Gated Integrated Loudness across the entire timeline
    pub fn integrated_lufs(&self) -> f32 {
        let n_sub = self.sub_blocks_100ms.len();
        if n_sub < 4 {
            return self.momentary_lufs();
        }

        // Build 400ms overlapping blocks (step = 100ms)
        let mut block_powers = Vec::with_capacity(n_sub - 3);
        for i in 0..=(n_sub - 4) {
            let sum: f64 = self.sub_blocks_100ms[i..i + 4].iter().sum();
            let block_power = sum / 4.0;
            block_powers.push(block_power);
        }

        // Stage 1: Absolute Gating Threshold (-70 LUFS)
        let abs_threshold_power = 10.0f64.powf((-70.0 + 0.691) / 10.0);
        let mut ungated_sum = 0.0;
        let mut ungated_count = 0;

        for &p in &block_powers {
            if p >= abs_threshold_power {
                ungated_sum += p;
                ungated_count += 1;
            }
        }

        if ungated_count == 0 {
            return -70.0;
        }

        let ungated_mean = ungated_sum / ungated_count as f64;
        let ungated_lufs = Self::power_to_lufs(ungated_mean);

        // Stage 2: Relative Gating Threshold (Gamma_r = ungated_lufs - 10.0 LU)
        let rel_threshold_lufs = ungated_lufs - 10.0;
        let rel_threshold_power = 10.0f64.powf(((rel_threshold_lufs + 0.691) as f64) / 10.0);

        let mut gated_sum = 0.0;
        let mut gated_count = 0;

        for &p in &block_powers {
            if p >= abs_threshold_power && p >= rel_threshold_power {
                gated_sum += p;
                gated_count += 1;
            }
        }

        if gated_count == 0 {
            return -70.0;
        }

        let gated_mean = gated_sum / gated_count as f64;
        Self::power_to_lufs(gated_mean)
    }

    #[inline]
    fn power_to_lufs(mean_power: f64) -> f32 {
        if mean_power <= 1e-12 {
            -70.0
        } else {
            (-0.691 + 10.0 * mean_power.log10()).clamp(-70.0, 10.0) as f32
        }
    }

    #[inline]
    fn to_dbtp(val: f32) -> f32 {
        if val <= 1e-6 {
            -60.0
        } else {
            (20.0 * val.log10()).clamp(-60.0, 12.0)
        }
    }

    /// Query the complete current loudness, true-peak, and phase correlation readout
    pub fn get_readout(&self) -> LoudnessReadout {
        LoudnessReadout {
            momentary_lufs: self.momentary_lufs(),
            short_term_lufs: self.short_term_lufs(),
            integrated_lufs: self.integrated_lufs(),
            true_peak_l_dbtp: Self::to_dbtp(self.max_true_peak_l),
            true_peak_r_dbtp: Self::to_dbtp(self.max_true_peak_r),
            phase_correlation: self.current_correlation,
        }
    }
}

/// Calculate ITU-R BS.1770-4 Gated Integrated LUFS for an entire audio buffer
pub fn calculate_buffer_integrated_lufs(buffer: &crate::audio::buffer::AudioBuffer) -> f32 {
    let mut meter = EbuR128Meter::new(buffer.sample_rate as f32);
    let n = buffer.num_frames();
    if n == 0 {
        return -70.0;
    }

    const CHUNK_SIZE: usize = 4096;
    for chunk_start in (0..n).step_by(CHUNK_SIZE) {
        let chunk_end = (chunk_start + CHUNK_SIZE).min(n);
        let left_slice = if !buffer.channels.is_empty() {
            &buffer.channels[0][chunk_start..chunk_end]
        } else {
            &[]
        };
        let right_slice = if buffer.channels.len() > 1 {
            &buffer.channels[1][chunk_start..chunk_end]
        } else {
            left_slice
        };
        meter.process_block(left_slice, right_slice);
    }

    meter.integrated_lufs()
}
