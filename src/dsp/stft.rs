//! Streaming short-time Fourier transform with overlap-add resynthesis, shared by the spectral
//! modules. Square-root Hann windows at 75% overlap reconstruct the input exactly when the
//! spectrum is left untouched; the processing delay is one FFT length.

use std::f32::consts::PI;
use crate::audio::Radix2Fft;

pub const STFT_SIZE: usize = 2048;
pub const STFT_HOP: usize = STFT_SIZE / 4;
pub const STFT_BINS: usize = STFT_SIZE / 2 + 1;

/// One analysed frame: complex spectra of both channels (bins 0..=N/2 are meaningful)
pub struct SpectralFrame<'a> {
    pub re_l: &'a mut [f32],
    pub im_l: &'a mut [f32],
    pub re_r: &'a mut [f32],
    pub im_r: &'a mut [f32],
}

impl SpectralFrame<'_> {
    /// Multiply bin `k` of both channels by a real gain, keeping the spectrum conjugate-symmetric
    #[inline]
    pub fn scale_bin(&mut self, k: usize, gain: f32) {
        let n = self.re_l.len();
        self.re_l[k] *= gain;
        self.im_l[k] *= gain;
        self.re_r[k] *= gain;
        self.im_r[k] *= gain;
        if k > 0 && k < n / 2 {
            let m = n - k;
            self.re_l[m] *= gain;
            self.im_l[m] *= gain;
            self.re_r[m] *= gain;
            self.im_r[m] *= gain;
        }
    }

    #[inline]
    pub fn mag_l(&self, k: usize) -> f32 {
        (self.re_l[k] * self.re_l[k] + self.im_l[k] * self.im_l[k]).sqrt()
    }

    #[inline]
    pub fn mag_r(&self, k: usize) -> f32 {
        (self.re_r[k] * self.re_r[k] + self.im_r[k] * self.im_r[k]).sqrt()
    }
}

#[derive(Clone, Debug)]
pub struct StftProcessor {
    fft: Radix2Fft,
    window: Vec<f32>,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    fill: usize,
    re_l: Vec<f32>,
    im_l: Vec<f32>,
    re_r: Vec<f32>,
    im_r: Vec<f32>,
}

impl Default for StftProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl StftProcessor {
    pub fn new() -> Self {
        let n = STFT_SIZE;
        let window = (0..n)
            .map(|i| (0.5 * (1.0 - (2.0 * PI * i as f32 / n as f32).cos())).sqrt())
            .collect();
        Self {
            fft: Radix2Fft::new(n),
            window,
            in_l: vec![0.0; n],
            in_r: vec![0.0; n],
            out_l: vec![0.0; n],
            out_r: vec![0.0; n],
            fill: 0,
            re_l: vec![0.0; n],
            im_l: vec![0.0; n],
            re_r: vec![0.0; n],
            im_r: vec![0.0; n],
        }
    }

    pub fn latency(&self) -> usize {
        STFT_SIZE
    }

    pub fn reset(&mut self) {
        self.in_l.fill(0.0);
        self.in_r.fill(0.0);
        self.out_l.fill(0.0);
        self.out_r.fill(0.0);
        self.fill = 0;
    }

    /// Stream audio through, calling `on_frame` once per hop with the analysed spectra
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], on_frame: &mut dyn FnMut(&mut SpectralFrame)) {
        let n = STFT_SIZE;
        let hop = STFT_HOP;
        // Periodic sqrt-Hann analysis * synthesis sums to 2.0 at 75% overlap
        let ola_scale = 0.5;

        for i in 0..left.len().min(right.len()) {
            let write = n - hop + self.fill;
            self.in_l[write] = left[i];
            self.in_r[write] = right[i];
            left[i] = self.out_l[self.fill];
            right[i] = self.out_r[self.fill];
            self.fill += 1;

            if self.fill == hop {
                self.fill = 0;
                for k in 0..n {
                    self.re_l[k] = self.in_l[k] * self.window[k];
                    self.re_r[k] = self.in_r[k] * self.window[k];
                }
                self.im_l.fill(0.0);
                self.im_r.fill(0.0);
                self.fft.forward(&mut self.re_l, &mut self.im_l);
                self.fft.forward(&mut self.re_r, &mut self.im_r);

                {
                    let mut frame = SpectralFrame {
                        re_l: &mut self.re_l,
                        im_l: &mut self.im_l,
                        re_r: &mut self.re_r,
                        im_r: &mut self.im_r,
                    };
                    on_frame(&mut frame);
                }

                self.fft.inverse(&mut self.re_l, &mut self.im_l);
                self.fft.inverse(&mut self.re_r, &mut self.im_r);

                self.out_l.copy_within(hop.., 0);
                self.out_r.copy_within(hop.., 0);
                self.out_l[n - hop..].fill(0.0);
                self.out_r[n - hop..].fill(0.0);
                for k in 0..n {
                    let w = self.window[k] * ola_scale;
                    self.out_l[k] += self.re_l[k] * w;
                    self.out_r[k] += self.re_r[k] * w;
                }
                self.in_l.copy_within(hop.., 0);
                self.in_r.copy_within(hop.., 0);
            }
        }
    }
}

/// Centre frequency of an FFT bin
#[inline]
pub fn bin_freq(k: usize, sample_rate: f32) -> f32 {
    k as f32 * sample_rate / STFT_SIZE as f32
}
