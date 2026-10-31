//! Linear-phase 2x / 4x oversampling for nonlinear modules (saturation, limiting), so the
//! harmonics they create above the original Nyquist are filtered out instead of aliasing back.
//!
//! Each 2x stage zero-stuffs and filters with a 49-tap Kaiser-windowed half-band low-pass on the
//! way up, and filters and decimates on the way down. The delay is a whole number of base-rate
//! samples (24 for 2x, 36 for 4x), so the chain's latency compensation stays exact.

use std::f64::consts::PI;

const TAPS: usize = 49;
const KAISER_BETA: f64 = 9.0;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..50 {
        term *= (half / k as f64) * (half / k as f64);
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

/// Half-band low-pass (cutoff at a quarter of the high rate), unity DC gain
fn halfband_coeffs() -> [f32; TAPS] {
    let mut h = [0.0f32; TAPS];
    let m = (TAPS - 1) as f64 / 2.0;
    let denom = bessel_i0(KAISER_BETA);
    let mut sum = 0.0;
    for (n, c) in h.iter_mut().enumerate() {
        let t = n as f64 - m;
        let sinc = if t == 0.0 { 0.5 } else { (PI * 0.5 * t).sin() / (PI * t) };
        let w = bessel_i0(KAISER_BETA * (1.0 - (t / m).powi(2)).max(0.0).sqrt()) / denom;
        *c = (sinc * w) as f32;
        sum += sinc * w;
    }
    for c in h.iter_mut() {
        *c = (*c as f64 / sum) as f32;
    }
    h
}

/// FIR state for one channel, as a doubled ring buffer so each output is one contiguous dot product
#[derive(Clone, Debug)]
struct FirState {
    buf: Vec<f32>,
    pos: usize,
}

impl FirState {
    fn new() -> Self {
        Self { buf: vec![0.0; TAPS * 2], pos: 0 }
    }

    fn reset(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }

    #[inline]
    fn push(&mut self, x: f32) {
        self.buf[self.pos] = x;
        self.buf[self.pos + TAPS] = x;
        self.pos = (self.pos + 1) % TAPS;
    }

    /// Filter output for the samples pushed so far (newest last)
    #[inline]
    fn output(&self, h: &[f32; TAPS]) -> f32 {
        let window = &self.buf[self.pos..self.pos + TAPS];
        // Oldest sample pairs with the last coefficient; the filter is symmetric so order is moot
        window.iter().zip(h.iter()).map(|(x, c)| x * c).sum()
    }
}

#[derive(Clone, Debug)]
struct Stage {
    up: [FirState; 2],
    down: [FirState; 2],
}

impl Stage {
    fn new() -> Self {
        Self { up: [FirState::new(), FirState::new()], down: [FirState::new(), FirState::new()] }
    }

    fn reset(&mut self) {
        for f in self.up.iter_mut().chain(self.down.iter_mut()) {
            f.reset();
        }
    }
}

#[derive(Clone, Debug)]
pub struct Oversampler {
    factor: usize,
    stages: Vec<Stage>,
    h: [f32; TAPS],
    /// Working buffers per stage level: index 0 = 2x, 1 = 4x
    work: Vec<[Vec<f32>; 2]>,
}

impl Oversampler {
    /// `factor` is 1, 2 or 4
    pub fn new(factor: usize) -> Self {
        let factor: usize = match factor {
            4 => 4,
            2 => 2,
            _ => 1,
        };
        let n_stages = factor.trailing_zeros() as usize;
        Self {
            factor,
            stages: (0..n_stages).map(|_| Stage::new()).collect(),
            h: halfband_coeffs(),
            work: (0..n_stages).map(|_| [Vec::new(), Vec::new()]).collect(),
        }
    }

    pub fn factor(&self) -> usize {
        self.factor
    }

    /// Round-trip delay in base-rate samples
    pub fn latency(&self) -> usize {
        // Each stage adds TAPS-1 samples at its own (high) rate: 48 at 2x = 24 base samples,
        // plus 48 at 4x = 12 base samples
        let mut total = 0;
        for s in 0..self.stages.len() {
            total += (TAPS - 1) / (2 << s);
        }
        total
    }

    pub fn reset(&mut self) {
        for s in self.stages.iter_mut() {
            s.reset();
        }
    }

    /// Run `f` on the signal at `factor` times the rate; with factor 1 it runs in place
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], f: &mut dyn FnMut(&mut [f32], &mut [f32])) {
        let n = left.len().min(right.len());
        if self.factor == 1 || n == 0 {
            f(left, right);
            return;
        }
        let Self { stages, h, work, .. } = self;
        let levels = stages.len();
        for (lvl, bufs) in work.iter_mut().enumerate() {
            let len = n << (lvl + 1);
            for b in bufs.iter_mut() {
                if b.len() < len {
                    b.resize(len, 0.0);
                }
            }
        }

        // Up: each stage doubles the rate. Zero-stuffing halves the level; x2 restores it.
        for lvl in 0..levels {
            let (lower, upper) = work.split_at_mut(lvl);
            for ch in 0..2 {
                let src: &[f32] = if lvl == 0 {
                    if ch == 0 { &left[..n] } else { &right[..n] }
                } else {
                    &lower[lvl - 1][ch][..n << lvl]
                };
                let dst = &mut upper[0][ch];
                let fir = &mut stages[lvl].up[ch];
                for (i, &x) in src.iter().enumerate() {
                    fir.push(x * 2.0);
                    dst[2 * i] = fir.output(h);
                    fir.push(0.0);
                    dst[2 * i + 1] = fir.output(h);
                }
            }
        }

        {
            let len = n << levels;
            let [a, b] = &mut work[levels - 1];
            f(&mut a[..len], &mut b[..len]);
        }

        // Down: filter, keep every other sample
        for lvl in (0..levels).rev() {
            let (lower, upper) = work.split_at_mut(lvl);
            for ch in 0..2 {
                let src = &upper[0][ch][..n << (lvl + 1)];
                let fir = &mut stages[lvl].down[ch];
                let dst: &mut [f32] = if lvl == 0 {
                    if ch == 0 { &mut left[..n] } else { &mut right[..n] }
                } else {
                    &mut lower[lvl - 1][ch][..n << lvl]
                };
                for (i, out) in dst.iter_mut().enumerate() {
                    fir.push(src[2 * i]);
                    *out = fir.output(h);
                    fir.push(src[2 * i + 1]);
                }
            }
        }
    }
}
