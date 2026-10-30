/// Mastering Maximizer with Lookahead & 4x True Peak Inter-Sample Peak Limiting
#[derive(Clone, Debug)]
pub struct MasteringMaximizer {
    pub enabled: bool,
    pub threshold_db: f32, // Input gain drive / threshold
    pub ceiling_db: f32,   // True peak ceiling (default -0.1 dBTP or -1.0 dBTP)
    pub release_ms: f32,
    pub irc_adaptive: bool,
    sample_rate: f32,
    
    // Lookahead ring buffers (2 ms lookahead)
    lookahead_len: usize,
    lookahead_l: Vec<f32>,
    lookahead_r: Vec<f32>,
    lookahead_pos: usize,

    // Envelope and gain reduction state
    envelope: f32,
    pub gain_reduction_db: f32,

    // 4x True Peak detection history
    tp_history_l: [f32; 4],
    tp_history_r: [f32; 4],
}

impl MasteringMaximizer {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(8000.0);
        let lookahead_len = ((sr * 0.002) as usize).max(8); // 2 ms lookahead

        Self {
            enabled: true,
            threshold_db: -3.5, // Drive gain
            ceiling_db: -0.1,   // Mastering ceiling
            release_ms: 50.0,
            irc_adaptive: true,
            sample_rate: sr,
            lookahead_len,
            lookahead_l: vec![0.0; lookahead_len],
            lookahead_r: vec![0.0; lookahead_len],
            lookahead_pos: 0,
            envelope: 0.0,
            gain_reduction_db: 0.0,
            tp_history_l: [0.0; 4],
            tp_history_r: [0.0; 4],
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = sample_rate.max(8000.0);
        self.sample_rate = sr;
        self.lookahead_len = ((sr * 0.002) as usize).max(8);
        self.lookahead_l = vec![0.0; self.lookahead_len];
        self.lookahead_r = vec![0.0; self.lookahead_len];
        self.lookahead_pos = 0;
        self.reset();
    }

    pub fn latency_samples(&self) -> usize {
        self.lookahead_len
    }

    pub fn reset(&mut self) {
        self.lookahead_l.fill(0.0);
        self.lookahead_r.fill(0.0);
        self.lookahead_pos = 0;
        self.envelope = 0.0;
        self.gain_reduction_db = 0.0;
        self.tp_history_l = [0.0; 4];
        self.tp_history_r = [0.0; 4];
    }

    #[inline]
    fn compute_true_peak(&mut self, in_l: f32, in_r: f32) -> f32 {
        self.tp_history_l[3] = self.tp_history_l[2];
        self.tp_history_l[2] = self.tp_history_l[1];
        self.tp_history_l[1] = self.tp_history_l[0];
        self.tp_history_l[0] = in_l;

        self.tp_history_r[3] = self.tp_history_r[2];
        self.tp_history_r[2] = self.tp_history_r[1];
        self.tp_history_r[1] = self.tp_history_r[0];
        self.tp_history_r[0] = in_r;

        let eval_tp = |h: &[f32; 4]| -> f32 {
            let p0 = h[3];
            let p1 = h[2];
            let p2 = h[1];
            let p3 = h[0];
            let mut peak = p1.abs().max(p2.abs());

            for &t in &[0.25f32, 0.5f32, 0.75f32] {
                let t2 = t * t;
                let t3 = t2 * t;
                let val = 0.5 * (
                    (2.0 * p1) +
                    (-p0 + p2) * t +
                    (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2 +
                    (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3
                );
                let av = val.abs();
                if av > peak {
                    peak = av;
                }
            }
            peak
        };

        eval_tp(&self.tp_history_l).max(eval_tp(&self.tp_history_r))
    }

    pub fn process_block(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.enabled {
            return;
        }

        let in_gain = 10.0f32.powf(-self.threshold_db / 20.0);
        let ceiling = 10.0f32.powf(self.ceiling_db / 20.0);
        let base_rel_coeff = (-1.0 / (self.release_ms.max(1.0) * 0.001 * self.sample_rate)).exp();

        let n = left.len().min(right.len());
        for i in 0..n {
            // Apply input drive gain
            let dry_l = left[i] * in_gain;
            let dry_r = right[i] * in_gain;

            // 4x True-Peak Inter-Sample Peak measurement
            let tp_peak = self.compute_true_peak(dry_l, dry_r);

            // IRC adaptive release control: fast release for sharp transients, slower for sustained bass
            let rel_coeff = if self.irc_adaptive {
                if tp_peak > 1.4 {
                    base_rel_coeff.powf(0.6) // Faster release for intense peaks
                } else {
                    base_rel_coeff
                }
            } else {
                base_rel_coeff
            };

            // Attack is instantaneous with lookahead
            if tp_peak > self.envelope {
                self.envelope = tp_peak;
            } else {
                self.envelope = tp_peak + rel_coeff * (self.envelope - tp_peak);
            }

            // Lookahead ring buffer extraction
            let delayed_l = self.lookahead_l[self.lookahead_pos];
            let delayed_r = self.lookahead_r[self.lookahead_pos];

            self.lookahead_l[self.lookahead_pos] = dry_l;
            self.lookahead_r[self.lookahead_pos] = dry_r;
            self.lookahead_pos = (self.lookahead_pos + 1) % self.lookahead_len;

            // Compute gain attenuation
            let gain = if self.envelope > ceiling {
                ceiling / self.envelope
            } else {
                1.0
            };

            self.gain_reduction_db = (20.0 * gain.log10()).min(0.0);

            // Apply limiter output
            left[i] = (delayed_l * gain).clamp(-ceiling, ceiling);
            right[i] = (delayed_r * gain).clamp(-ceiling, ceiling);
        }
    }
}

