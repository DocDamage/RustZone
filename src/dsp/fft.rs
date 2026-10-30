use std::f32::consts::PI;

pub const FFT_SIZE: usize = 1024;
pub const NUM_SPECTRUM_BINS: usize = 128; // Log-spaced display bins from 20 Hz to 20 kHz

#[derive(Clone, Debug)]
pub struct SpectralAnalyzer {
    sample_rate: f32,
    input_ring: Vec<f32>,
    ring_pos: usize,
    hann_window: [f32; FFT_SIZE],
    twiddles_cos: [f32; FFT_SIZE / 2],
    twiddles_sin: [f32; FFT_SIZE / 2],
    bit_rev: [usize; FFT_SIZE],
    
    // Log-spaced bin mapping: (fft_bin_start, fft_bin_end)
    bin_ranges: [(usize, usize); NUM_SPECTRUM_BINS],
    
    // Smoothed spectrum magnitudes in dB (-90 dB to +12 dB)
    smooth_db: [f32; NUM_SPECTRUM_BINS],
}

impl SpectralAnalyzer {
    pub fn new(sample_rate: f32) -> Self {
        let mut analyzer = Self {
            sample_rate: sample_rate.max(8000.0),
            input_ring: vec![0.0; FFT_SIZE],
            ring_pos: 0,
            hann_window: [0.0; FFT_SIZE],
            twiddles_cos: [0.0; FFT_SIZE / 2],
            twiddles_sin: [0.0; FFT_SIZE / 2],
            bit_rev: [0; FFT_SIZE],
            bin_ranges: [(0, 0); NUM_SPECTRUM_BINS],
            smooth_db: [-90.0; NUM_SPECTRUM_BINS],
        };

        analyzer.init_tables();
        analyzer.compute_bin_ranges();
        analyzer
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(8000.0);
        self.compute_bin_ranges();
        self.reset();
    }

    pub fn reset(&mut self) {
        self.input_ring.fill(0.0);
        self.ring_pos = 0;
        self.smooth_db.fill(-90.0);
    }

    fn init_tables(&mut self) {
        // 1. Hann Window
        for i in 0..FFT_SIZE {
            self.hann_window[i] = 0.5 * (1.0 - (2.0 * PI * (i as f32) / (FFT_SIZE as f32)).cos());
        }

        // 2. Twiddle Factors
        for i in 0..(FFT_SIZE / 2) {
            let angle = -2.0 * PI * (i as f32) / (FFT_SIZE as f32);
            self.twiddles_cos[i] = angle.cos();
            self.twiddles_sin[i] = angle.sin();
        }

        // 3. Bit-reversal table
        let num_bits = (FFT_SIZE as f32).log2() as usize;
        for i in 0..FFT_SIZE {
            let mut rev = 0;
            let mut val = i;
            for _ in 0..num_bits {
                rev = (rev << 1) | (val & 1);
                val >>= 1;
            }
            self.bit_rev[i] = rev;
        }
    }

    fn compute_bin_ranges(&mut self) {
        let min_freq = 20.0f32;
        let max_freq = (self.sample_rate * 0.48).min(22000.0);
        let freq_per_bin = self.sample_rate / FFT_SIZE as f32;

        let log_min = min_freq.ln();
        let log_max = max_freq.ln();

        for i in 0..NUM_SPECTRUM_BINS {
            let t0 = i as f32 / NUM_SPECTRUM_BINS as f32;
            let t1 = (i + 1) as f32 / NUM_SPECTRUM_BINS as f32;

            let f0 = (log_min + t0 * (log_max - log_min)).exp();
            let f1 = (log_min + t1 * (log_max - log_min)).exp();

            let k0 = ((f0 / freq_per_bin).floor() as usize).clamp(1, FFT_SIZE / 2 - 1);
            let k1 = ((f1 / freq_per_bin).ceil() as usize).clamp(k0 + 1, FFT_SIZE / 2);

            self.bin_ranges[i] = (k0, k1);
        }
    }

    /// Feed a stereo block into the analyzer
    pub fn feed_samples(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len().min(right.len());
        for i in 0..n {
            // Mono mixdown for spectral visualization
            let mono = (left[i] + right[i]) * 0.5;
            self.input_ring[self.ring_pos] = mono;
            self.ring_pos = (self.ring_pos + 1) % FFT_SIZE;
        }
    }

    /// Perform in-place Radix-2 FFT and compute the smoothed spectrum magnitudes
    pub fn update_spectrum(&mut self) {
        let mut re = [0.0f32; FFT_SIZE];
        let mut im = [0.0f32; FFT_SIZE];

        // 1. Unroll ring buffer into windowed array in bit-reversed order
        let start = self.ring_pos;
        for i in 0..FFT_SIZE {
            let ring_idx = (start + i) % FFT_SIZE;
            let windowed = self.input_ring[ring_idx] * self.hann_window[i];
            let rev_idx = self.bit_rev[i];
            re[rev_idx] = windowed;
            im[rev_idx] = 0.0;
        }

        // 2. In-place Cooley-Tukey Radix-2 FFT
        let mut len = 2;
        while len <= FFT_SIZE {
            let half = len / 2;
            let step = FFT_SIZE / len;

            let mut i = 0;
            while i < FFT_SIZE {
                for j in 0..half {
                    let tw_idx = j * step;
                    let cos_w = self.twiddles_cos[tw_idx];
                    let sin_w = self.twiddles_sin[tw_idx];

                    let u_re = re[i + j];
                    let u_im = im[i + j];

                    let v_re = re[i + j + half] * cos_w - im[i + j + half] * sin_w;
                    let v_im = re[i + j + half] * sin_w + im[i + j + half] * cos_w;

                    re[i + j] = u_re + v_re;
                    im[i + j] = u_im + v_im;
                    re[i + j + half] = u_re - v_re;
                    im[i + j + half] = u_im - v_im;
                }
                i += len;
            }
            len <<= 1;
        }

        // 3. Compute magnitude spectrum and map to log-spaced bins
        for bin_idx in 0..NUM_SPECTRUM_BINS {
            let (k0, k1) = self.bin_ranges[bin_idx];
            let mut max_mag = 0.0f32;

            for k in k0..k1 {
                let mag_sq = re[k] * re[k] + im[k] * im[k];
                if mag_sq > max_mag {
                    max_mag = mag_sq;
                }
            }

            let mag = (max_mag.sqrt() / (FFT_SIZE as f32 * 0.25)).max(1e-5);
            let db = (20.0 * mag.log10()).clamp(-90.0, 12.0);

            // Ballistic smoothing: instant attack, smooth exponential decay
            let current = self.smooth_db[bin_idx];
            if db > current {
                self.smooth_db[bin_idx] = current + 0.65 * (db - current);
            } else {
                self.smooth_db[bin_idx] = current + 0.12 * (db - current);
            }
        }
    }

    /// Retrieve the current 128-bin logarithmic frequency curve in dB
    pub fn get_spectrum_db(&self) -> &[f32; NUM_SPECTRUM_BINS] {
        &self.smooth_db
    }
}

