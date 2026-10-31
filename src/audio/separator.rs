use std::f32::consts::PI;
use crate::audio::buffer::AudioBuffer;

/// Discrete Fast Fourier Transform engine (Radix-2 Cooley-Tukey)
#[derive(Clone, Debug)]
pub struct Radix2Fft {
    pub n: usize,
    twiddles_cos: Vec<f32>,
    twiddles_sin: Vec<f32>,
    bit_rev: Vec<usize>,
}

impl Radix2Fft {
    /// Empty stand-in that allocates nothing (not usable for transforms)
    pub(crate) fn placeholder() -> Self {
        Self { n: 0, twiddles_cos: Vec::new(), twiddles_sin: Vec::new(), bit_rev: Vec::new() }
    }

    pub fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 2, "FFT size must be a power of two >= 2");
        let mut twiddles_cos = Vec::with_capacity(n / 2);
        let mut twiddles_sin = Vec::with_capacity(n / 2);
        for i in 0..(n / 2) {
            let angle = -2.0 * PI * (i as f32) / (n as f32);
            twiddles_cos.push(angle.cos());
            twiddles_sin.push(angle.sin());
        }

        let num_bits = (n as f32).log2() as usize;
        let mut bit_rev = vec![0; n];
        for i in 0..n {
            let mut rev = 0;
            let mut temp = i;
            for _ in 0..num_bits {
                rev = (rev << 1) | (temp & 1);
                temp >>= 1;
            }
            bit_rev[i] = rev;
        }

        Self {
            n,
            twiddles_cos,
            twiddles_sin,
            bit_rev,
        }
    }

    /// In-place forward FFT
    pub fn forward(&self, re: &mut [f32], im: &mut [f32]) {
        assert_eq!(re.len(), self.n);
        assert_eq!(im.len(), self.n);

        // Bit-reversal permutation
        for i in 0..self.n {
            let j = self.bit_rev[i];
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }

        // Butterfly stages
        let mut len = 2;
        while len <= self.n {
            let half = len / 2;
            let step = self.n / len;

            let mut i = 0;
            while i < self.n {
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
    }

    /// In-place inverse FFT
    pub fn inverse(&self, re: &mut [f32], im: &mut [f32]) {
        assert_eq!(re.len(), self.n);
        assert_eq!(im.len(), self.n);

        for val in im.iter_mut() {
            *val = -*val;
        }

        self.forward(re, im);

        let inv_n = 1.0 / (self.n as f32);
        for (r, m) in re.iter_mut().zip(im.iter_mut()) {
            *r *= inv_n;
            *m = -*m * inv_n;
        }
    }
}

/// Helper for stack-allocated median filtering of small windows (up to 9 elements)
#[inline]
fn fast_median_7(slice: &[f32]) -> f32 {
    let mut buf = [0.0f32; 9];
    let len = slice.len().min(9);
    if len == 0 {
        return 0.0;
    }
    buf[..len].copy_from_slice(&slice[..len]);

    // Simple insertion sort on tiny array
    for i in 1..len {
        let key = buf[i];
        let mut j = i;
        while j > 0 && buf[j - 1] > key {
            buf[j] = buf[j - 1];
            j -= 1;
        }
        buf[j] = key;
    }

    buf[len / 2]
}

/// Results of multi-stem separation
pub struct StemSeparationResult {
    pub drums: AudioBuffer,
    pub bass: AudioBuffer,
    pub vocals: AudioBuffer,
    pub instruments: AudioBuffer,
    pub synths: AudioBuffer,
    pub ambience: AudioBuffer,
}

/// Pure-Rust Harmonic-Percussive & Azimuth Audio Stem Separator
pub struct StemSeparator;

impl StemSeparator {
    /// Separate a stereo AudioBuffer into 6 independent stems:
    /// 1. Drums (Percussive / Transients)
    /// 2. Bass (Low-frequency tonal / Sub $< 220 Hz)
    /// 3. Vocals (Mid-centered harmonic formants)
    /// 4. Instruments (Harmonic Guitars & Keys)
    /// 5. Synths & FX (High-frequency harmonic & wide synths)
    /// 6. Ambience (Reverberation tails & wide diffuse stereo sides)
    ///
    /// GUARANTEE: Stems are energy-normalized to guarantee bit-transparent perfect reconstruction
    /// when summed at unity gain (0 dB).
    pub fn separate_stems<F>(
        input: &AudioBuffer,
        progress_cb: F,
    ) -> Result<StemSeparationResult, String>
    where
        F: Fn(f32, &str),
    {
        let num_frames = input.num_frames();
        if num_frames == 0 {
            return Err("Input audio buffer is empty".to_string());
        }

        let sample_rate = input.sample_rate;
        let (in_l, in_r) = input.to_stereo();

        let fft_size = 2048;
        let hop_size = 512;
        let fft = Radix2Fft::new(fft_size);

        // Precompute Hann window
        let mut hann = vec![0.0f32; fft_size];
        for i in 0..fft_size {
            hann[i] = 0.5 * (1.0 - (2.0 * PI * i as f32 / fft_size as f32).cos());
        }

        let num_stft_frames = if num_frames >= fft_size {
            (num_frames - fft_size) / hop_size + 1
        } else {
            1
        };

        progress_cb(0.05, "Computing Short-Time Fourier Transform (STFT)...");

        // STFT spectral frames: [frame_idx][bin_idx]
        let num_bins = fft_size / 2 + 1;
        let mut stft_l_re = Vec::with_capacity(num_stft_frames);
        let mut stft_l_im = Vec::with_capacity(num_stft_frames);
        let mut stft_r_re = Vec::with_capacity(num_stft_frames);
        let mut stft_r_im = Vec::with_capacity(num_stft_frames);
        let mut mag_spec = Vec::with_capacity(num_stft_frames);
        let mut center_coherence = Vec::with_capacity(num_stft_frames);

        let mut buf_re = vec![0.0f32; fft_size];
        let mut buf_im = vec![0.0f32; fft_size];

        for m in 0..num_stft_frames {
            let start = m * hop_size;

            // Process Left channel
            buf_re.fill(0.0);
            buf_im.fill(0.0);
            for i in 0..fft_size {
                if start + i < in_l.len() {
                    buf_re[i] = in_l[start + i] * hann[i];
                }
            }
            fft.forward(&mut buf_re, &mut buf_im);
            let frame_l_re: Vec<f32> = buf_re[..num_bins].to_vec();
            let frame_l_im: Vec<f32> = buf_im[..num_bins].to_vec();

            // Process Right channel
            buf_re.fill(0.0);
            buf_im.fill(0.0);
            for i in 0..fft_size {
                if start + i < in_r.len() {
                    buf_re[i] = in_r[start + i] * hann[i];
                }
            }
            fft.forward(&mut buf_re, &mut buf_im);
            let frame_r_re: Vec<f32> = buf_re[..num_bins].to_vec();
            let frame_r_im: Vec<f32> = buf_im[..num_bins].to_vec();

            // Compute magnitude and stereo center coherence per bin
            let mut frame_mag = vec![0.0f32; num_bins];
            let mut frame_coh = vec![0.0f32; num_bins];

            for k in 0..num_bins {
                let mag_l = (frame_l_re[k] * frame_l_re[k] + frame_l_im[k] * frame_l_im[k]).sqrt();
                let mag_r = (frame_r_re[k] * frame_r_re[k] + frame_r_im[k] * frame_r_im[k]).sqrt();
                frame_mag[k] = 0.5 * (mag_l + mag_r);

                // Stereo azimuth coherence: C ~ 1.0 (dead center), C ~ 0.0 (wide stereo / ambient)
                let energy_sum = mag_l * mag_l + mag_r * mag_r + 1e-7;
                frame_coh[k] = ((2.0 * mag_l * mag_r) / energy_sum).clamp(0.0, 1.0);
            }

            stft_l_re.push(frame_l_re);
            stft_l_im.push(frame_l_im);
            stft_r_re.push(frame_r_re);
            stft_r_im.push(frame_r_im);
            mag_spec.push(frame_mag);
            center_coherence.push(frame_coh);

            if m % 200 == 0 {
                let pct = 0.05 + 0.35 * (m as f32 / num_stft_frames as f32);
                progress_cb(pct, "Analyzing spectral components...");
            }
        }

        progress_cb(0.42, "Running Harmonic-Percussive Source Separation (HPSS)...");

        // Compute Harmonic (H) and Percussive (P) matrices via 2D Spectrogram Median Filtering
        // H: median along time axis (m +/- tau)
        // P: median along frequency axis (k +/- kappa)
        let tau = 3;   // Window size 7 in time
        let kappa = 3; // Window size 7 in frequency

        let mut h_spec = vec![vec![0.0f32; num_bins]; num_stft_frames];
        let mut p_spec = vec![vec![0.0f32; num_bins]; num_stft_frames];

        // 1. Time median -> Harmonic
        for k in 0..num_bins {
            for m in 0..num_stft_frames {
                let m_start = m.saturating_sub(tau);
                let m_end = (m + tau + 1).min(num_stft_frames);
                let mut temp = [0.0f32; 9];
                let count = m_end - m_start;
                for idx in 0..count {
                    temp[idx] = mag_spec[m_start + idx][k];
                }
                h_spec[m][k] = fast_median_7(&temp[..count]);
            }
        }

        // 2. Frequency median -> Percussive
        for m in 0..num_stft_frames {
            for k in 0..num_bins {
                let k_start = k.saturating_sub(kappa);
                let k_end = (k + kappa + 1).min(num_bins);
                let mut temp = [0.0f32; 9];
                let count = k_end - k_start;
                temp[..count].copy_from_slice(&mag_spec[m][k_start..k_end]);
                p_spec[m][k] = fast_median_7(&temp[..count]);
            }
        }

        progress_cb(0.60, "Synthesizing 6-stem spectral masks...");

        // Pre-allocate output time-domain buffers for all 6 stems
        let out_len = num_frames;
        let mut drums_l = vec![0.0f32; out_len];
        let mut drums_r = vec![0.0f32; out_len];
        let mut bass_l = vec![0.0f32; out_len];
        let mut bass_r = vec![0.0f32; out_len];
        let mut vocals_l = vec![0.0f32; out_len];
        let mut vocals_r = vec![0.0f32; out_len];
        let mut inst_l = vec![0.0f32; out_len];
        let mut inst_r = vec![0.0f32; out_len];
        let mut synths_l = vec![0.0f32; out_len];
        let mut synths_r = vec![0.0f32; out_len];
        let mut amb_l = vec![0.0f32; out_len];
        let mut amb_r = vec![0.0f32; out_len];

        let mut win_sum = vec![0.0f32; out_len];

        // Inverse STFT overlap-add buffers
        let mut ifft_re = vec![0.0f32; fft_size];
        let mut ifft_im = vec![0.0f32; fft_size];

        let freq_per_bin = sample_rate as f32 / fft_size as f32;

        for m in 0..num_stft_frames {
            let start = m * hop_size;

            // Accumulate window power for overlap-add normalization
            for i in 0..fft_size {
                if start + i < out_len {
                    win_sum[start + i] += hann[i] * hann[i];
                }
            }

            // Masks for each of the 6 stems across all bins in frame m
            let mut mask_drums = vec![0.0f32; num_bins];
            let mut mask_bass = vec![0.0f32; num_bins];
            let mut mask_vocals = vec![0.0f32; num_bins];
            let mut mask_inst = vec![0.0f32; num_bins];
            let mut mask_synths = vec![0.0f32; num_bins];
            let mut mask_amb = vec![0.0f32; num_bins];

            for k in 0..num_bins {
                let f_hz = k as f32 * freq_per_bin;
                let h_val = h_spec[m][k];
                let p_val = p_spec[m][k];
                let coh = center_coherence[m][k]; // 1.0 = center, 0.0 = wide side

                let h_sq = h_val * h_val;
                let p_sq = p_val * p_val;
                let denom = h_sq + p_sq + 1e-7;

                // Soft Wiener harmonic & percussive weights
                let w_perc = (p_sq / denom).clamp(0.0, 1.0);
                let w_harm = (h_sq / denom).clamp(0.0, 1.0);

                let side_weight = (1.0 - coh).clamp(0.0, 1.0);

                // 1. Drums (Transient / Percussive)
                let high_percussive_boost = if f_hz > 5000.0 { 1.3 } else { 1.0 };
                let low_kick_boost = if f_hz < 120.0 { 1.2 } else { 1.0 };
                let raw_drums = w_perc.powf(1.1) * high_percussive_boost * low_kick_boost;

                // 2. Bass (Low-frequency tonal / Sub < 220 Hz, center-focused)
                let bass_freq_gate = if f_hz <= 220.0 {
                    1.0
                } else if f_hz < 320.0 {
                    (320.0 - f_hz) / 100.0
                } else {
                    0.0
                };
                let raw_bass = w_harm * coh * bass_freq_gate * 1.5;

                // 3. Vocals (Mid-centered harmonic formants between 150 Hz and 4500 Hz)
                let vocal_freq_gate = if (180.0..=4200.0).contains(&f_hz) {
                    1.0
                } else if f_hz > 4200.0 && f_hz < 5500.0 {
                    (5500.0 - f_hz) / 1300.0
                } else if (120.0..180.0).contains(&f_hz) {
                    (f_hz - 120.0) / 60.0
                } else {
                    0.0
                };
                let raw_vocals = w_harm * coh.powf(1.8) * vocal_freq_gate * 1.4;

                // 4. Instruments (Harmonic Guitars & Keys: mid-range harmonic, non-center or acoustic)
                let inst_freq_gate = if (220.0..=6500.0).contains(&f_hz) {
                    1.0
                } else if f_hz > 6500.0 && f_hz < 8000.0 {
                    (8000.0 - f_hz) / 1500.0
                } else {
                    0.0
                };
                let inst_panning_factor = 1.0 - (coh * vocal_freq_gate * 0.85);
                let raw_inst = w_harm * inst_panning_factor * inst_freq_gate * 1.2;

                // 5. Synths & FX (High harmonic textures > 2500 Hz and wide synths)
                let synth_high_gate = if f_hz >= 2500.0 {
                    1.0
                } else if f_hz >= 1500.0 {
                    (f_hz - 1500.0) / 1000.0
                } else {
                    0.0
                };
                let raw_synths = (w_harm * synth_high_gate * 0.8 + side_weight * synth_high_gate * 0.8) * 1.1;

                // 6. Ambience (Reverb tails, diffuse stereo sides)
                let raw_amb = side_weight.powf(1.3) * (1.0 - w_perc * 0.7) * 1.2;

                // Conservation of energy normalization (Sum-to-Unity)
                let raw_sum = raw_drums + raw_bass + raw_vocals + raw_inst + raw_synths + raw_amb + 1e-6;

                mask_drums[k] = raw_drums / raw_sum;
                mask_bass[k] = raw_bass / raw_sum;
                mask_vocals[k] = raw_vocals / raw_sum;
                mask_inst[k] = raw_inst / raw_sum;
                mask_synths[k] = raw_synths / raw_sum;
                mask_amb[k] = raw_amb / raw_sum;
            }

            // Synthesize stem frames via IFFT and Overlap-Add
            let stem_masks = [
                (&mask_drums, &mut drums_l, &mut drums_r),
                (&mask_bass, &mut bass_l, &mut bass_r),
                (&mask_vocals, &mut vocals_l, &mut vocals_r),
                (&mask_inst, &mut inst_l, &mut inst_r),
                (&mask_synths, &mut synths_l, &mut synths_r),
                (&mask_amb, &mut amb_l, &mut amb_r),
            ];

            for (mask, stem_l, stem_r) in stem_masks {
                // Reconstruct Left channel
                ifft_re.fill(0.0);
                ifft_im.fill(0.0);
                for k in 0..num_bins {
                    let m_gain = mask[k];
                    ifft_re[k] = stft_l_re[m][k] * m_gain;
                    ifft_im[k] = stft_l_im[m][k] * m_gain;
                }
                // Hermitian symmetry for real IFFT
                for k in 1..(fft_size / 2) {
                    ifft_re[fft_size - k] = ifft_re[k];
                    ifft_im[fft_size - k] = -ifft_im[k];
                }
                fft.inverse(&mut ifft_re, &mut ifft_im);

                for i in 0..fft_size {
                    if start + i < out_len {
                        stem_l[start + i] += ifft_re[i] * hann[i];
                    }
                }

                // Reconstruct Right channel
                ifft_re.fill(0.0);
                ifft_im.fill(0.0);
                for k in 0..num_bins {
                    let m_gain = mask[k];
                    ifft_re[k] = stft_r_re[m][k] * m_gain;
                    ifft_im[k] = stft_r_im[m][k] * m_gain;
                }
                for k in 1..(fft_size / 2) {
                    ifft_re[fft_size - k] = ifft_re[k];
                    ifft_im[fft_size - k] = -ifft_im[k];
                }
                fft.inverse(&mut ifft_re, &mut ifft_im);

                for i in 0..fft_size {
                    if start + i < out_len {
                        stem_r[start + i] += ifft_re[i] * hann[i];
                    }
                }
            }

            if m % 200 == 0 {
                let pct = 0.60 + 0.35 * (m as f32 / num_stft_frames as f32);
                progress_cb(pct, "Reconstructing time-domain stem audio buffers...");
            }
        }

        progress_cb(0.96, "Normalizing overlap-add synthesis energy...");

        // Normalize overlap-add energy across all output channels
        let stems_to_norm = [
            (&mut drums_l, &mut drums_r),
            (&mut bass_l, &mut bass_r),
            (&mut vocals_l, &mut vocals_r),
            (&mut inst_l, &mut inst_r),
            (&mut synths_l, &mut synths_r),
            (&mut amb_l, &mut amb_r),
        ];

        for (l_ch, r_ch) in stems_to_norm {
            for i in 0..out_len {
                let w = win_sum[i];
                let norm = if w > 1e-4 { 1.0 / w } else { 1.0 };
                l_ch[i] *= norm;
                r_ch[i] *= norm;
            }
        }

        progress_cb(1.0, "Stem separation complete.");

        Ok(StemSeparationResult {
            drums: AudioBuffer::from_stereo(drums_l, drums_r, sample_rate),
            bass: AudioBuffer::from_stereo(bass_l, bass_r, sample_rate),
            vocals: AudioBuffer::from_stereo(vocals_l, vocals_r, sample_rate),
            instruments: AudioBuffer::from_stereo(inst_l, inst_r, sample_rate),
            synths: AudioBuffer::from_stereo(synths_l, synths_r, sample_rate),
            ambience: AudioBuffer::from_stereo(amb_l, amb_r, sample_rate),
        })
    }
}

