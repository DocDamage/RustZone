use std::f32::consts::PI;
use crate::audio::AudioBuffer;

/// Zeroth-order modified Bessel function of the first kind I_0(x) for Kaiser windowing
fn bessel_i0(x: f32) -> f32 {
    let mut sum = 1.0f32;
    let mut term = 1.0f32;
    let half_x = x * 0.5;

    for k in 1..25 {
        term *= (half_x / k as f32) * (half_x / k as f32);
        sum += term;
        if term < 1e-8 * sum {
            break;
        }
    }
    sum
}

/// Sinc function: sinc(x) = sin(pi * x) / (pi * x)
#[inline]
fn sinc(x: f32) -> f32 {
    if x.abs() < 1e-7 {
        1.0
    } else {
        let pix = PI * x;
        pix.sin() / pix
    }
}

/// Audiophile Band-Limited Sinc Resampler with Kaiser Windowing
pub struct SincResampler {
    filter_half_length: usize, // Half width of filter in input samples (e.g. 16 to 32)
    kaiser_beta: f32,          // Kaiser beta parameter (8.5 gives ~90 dB stopband attenuation)
}

impl Default for SincResampler {
    fn default() -> Self {
        Self {
            filter_half_length: 24,
            kaiser_beta: 8.5,
        }
    }
}

impl SincResampler {
    pub fn new(filter_half_length: usize, kaiser_beta: f32) -> Self {
        Self {
            filter_half_length: filter_half_length.max(8),
            kaiser_beta: kaiser_beta.max(1.0),
        }
    }

    /// Resample a stereo audio signal from source sample rate to destination sample rate
    pub fn resample_stereo(
        &self,
        in_left: &[f32],
        in_right: &[f32],
        from_sr: u32,
        to_sr: u32,
    ) -> (Vec<f32>, Vec<f32>) {
        if from_sr == to_sr || in_left.is_empty() {
            return (in_left.to_vec(), in_right.to_vec());
        }

        let ratio = to_sr as f64 / from_sr as f64;
        let out_frames = (in_left.len() as f64 * ratio).round() as usize;
        let mut out_l = Vec::with_capacity(out_frames);
        let mut out_r = Vec::with_capacity(out_frames);

        // Anti-aliasing cutoff in cycles per input sample, just under the lower Nyquist frequency
        // (6% transition band margin)
        let cutoff = 0.47 * (to_sr as f32 / from_sr as f32).min(1.0);

        let i0_beta = bessel_i0(self.kaiser_beta);
        let n = in_left.len();

        for out_idx in 0..out_frames {
            let in_pos = out_idx as f64 / ratio;
            let center_idx = in_pos.floor() as isize;

            let mut sum_l = 0.0f32;
            let mut sum_r = 0.0f32;
            let mut weight_sum = 0.0f32;

            let half_len = self.filter_half_length as isize;
            for offset in -half_len..=half_len {
                let sample_idx = center_idx + offset;
                if sample_idx >= 0 && (sample_idx as usize) < n {
                    let dt = (in_pos - sample_idx as f64) as f32;
                    let norm_t = dt / half_len as f32;

                    if norm_t.abs() <= 1.0 {
                        // Kaiser window: w(t) = I0(beta * sqrt(1 - (t/T)^2)) / I0(beta)
                        let kaiser_arg = (1.0 - norm_t * norm_t).max(0.0).sqrt();
                        let window = bessel_i0(self.kaiser_beta * kaiser_arg) / i0_beta;

                        // Filter impulse response: 2 * cutoff * sinc(2 * cutoff * dt) * window
                        let h = 2.0 * cutoff * sinc(2.0 * cutoff * dt) * window;

                        let idx = sample_idx as usize;
                        sum_l += in_left[idx] * h;
                        sum_r += in_right[idx] * h;
                        weight_sum += h;
                    }
                }
            }

            // Normalize energy to ensure unity DC gain
            let norm = if weight_sum.abs() > 1e-6 {
                1.0 / weight_sum
            } else {
                1.0
            };

            out_l.push((sum_l * norm).clamp(-1.5, 1.5));
            out_r.push((sum_r * norm).clamp(-1.5, 1.5));
        }

        (out_l, out_r)
    }

    /// Resample an entire AudioBuffer to target sample rate
    pub fn resample_buffer(&self, buf: &AudioBuffer, target_sr: u32) -> AudioBuffer {
        if buf.sample_rate == target_sr || buf.num_frames() == 0 {
            return buf.clone();
        }

        let (left, right) = buf.to_stereo();
        let (resampled_l, resampled_r) = self.resample_stereo(&left, &right, buf.sample_rate, target_sr);
        AudioBuffer::from_stereo(resampled_l, resampled_r, target_sr)
    }
}

