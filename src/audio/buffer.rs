#[derive(Clone, Copy, Debug, Default)]
pub struct WaveformPeak {
    pub min_l: f32,
    pub max_l: f32,
    pub min_r: f32,
    pub max_r: f32,
}

#[derive(Clone, Debug)]
pub struct AudioBuffer {
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: u32,
    cached_peaks: Vec<WaveformPeak>,
}

impl AudioBuffer {
    pub fn new(num_channels: usize, sample_rate: u32) -> Self {
        Self {
            channels: vec![Vec::new(); num_channels],
            sample_rate,
            cached_peaks: Vec::new(),
        }
    }

    pub fn from_stereo(left: Vec<f32>, right: Vec<f32>, sample_rate: u32) -> Self {
        let mut buf = Self {
            channels: vec![left, right],
            sample_rate,
            cached_peaks: Vec::new(),
        };
        buf.cached_peaks = buf.generate_waveform_peaks(440);
        buf
    }

    pub fn num_channels(&self) -> usize {
        self.channels.len()
    }

    pub fn num_frames(&self) -> usize {
        if self.channels.is_empty() {
            0
        } else {
            self.channels[0].len()
        }
    }

    pub fn duration_seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.num_frames() as f64 / self.sample_rate as f64
        }
    }

    #[inline]
    pub fn get_stereo_frame(&self, frame: usize) -> (f32, f32) {
        if frame >= self.num_frames() {
            return (0.0, 0.0);
        }
        let l = self.channels[0][frame];
        let r = if self.channels.len() > 1 {
            self.channels[1][frame]
        } else {
            l
        };
        (l, r)
    }

    /// High-precision linear interpolation between frames for pitch-perfect resampling
    #[inline]
    pub fn get_interpolated_frame(&self, pos: f64) -> (f32, f32) {
        let n = self.num_frames();
        if n == 0 || pos < 0.0 {
            return (0.0, 0.0);
        }
        let idx0 = pos.floor() as usize;
        if idx0 >= n {
            return (0.0, 0.0);
        }
        let frac = (pos - idx0 as f64) as f32;
        let (l0, r0) = self.get_stereo_frame(idx0);
        if idx0 + 1 >= n {
            return (l0, r0);
        }
        let (l1, r1) = self.get_stereo_frame(idx0 + 1);
        (l0 + frac * (l1 - l0), r0 + frac * (r1 - r0))
    }

    /// Convert mono/multi-channel to guaranteed stereo (2 channels)
    pub fn to_stereo(&self) -> (Vec<f32>, Vec<f32>) {
        if self.channels.is_empty() {
            return (Vec::new(), Vec::new());
        }
        if self.channels.len() == 1 {
            (self.channels[0].clone(), self.channels[0].clone())
        } else {
            (self.channels[0].clone(), self.channels[1].clone())
        }
    }

    /// Get pre-computed waveform peaks (default 440 bins)
    pub fn peaks(&self) -> &[WaveformPeak] {
        &self.cached_peaks
    }

    /// Generate stereo waveform peaks for a given number of horizontal visual bins
    pub fn generate_waveform_peaks(&self, num_bins: usize) -> Vec<WaveformPeak> {
        let n = self.num_frames();
        if n == 0 || num_bins == 0 {
            return Vec::new();
        }

        let frames_per_bin = (n as f64 / num_bins as f64).max(1.0);
        let mut peaks = Vec::with_capacity(num_bins);

        for bin in 0..num_bins {
            let start = (bin as f64 * frames_per_bin) as usize;
            let end = (((bin + 1) as f64 * frames_per_bin) as usize).min(n);
            if start >= n {
                peaks.push(WaveformPeak::default());
                continue;
            }

            let mut min_l = 0.0f32;
            let mut max_l = 0.0f32;
            let mut min_r = 0.0f32;
            let mut max_r = 0.0f32;

            for f in start..end {
                let (l, r) = self.get_stereo_frame(f);
                if l < min_l { min_l = l; }
                if l > max_l { max_l = l; }
                if r < min_r { min_r = r; }
                if r > max_r { max_r = r; }
            }

            peaks.push(WaveformPeak {
                min_l: min_l.clamp(-1.0, 1.0),
                max_l: max_l.clamp(-1.0, 1.0),
                min_r: min_r.clamp(-1.0, 1.0),
                max_r: max_r.clamp(-1.0, 1.0),
            });
        }

        peaks
    }
}
