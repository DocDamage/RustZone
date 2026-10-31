//! Quality-control report for an exported master: loudness (integrated, range, maxima), true
//! peak, clipping, phase and mono compatibility, DC offset and silence at the ends. Lossy files
//! are measured as decoded, which is what listeners get (encoding adds inter-sample overs).

use std::f32::consts::PI;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::buffer::AudioBuffer;
use super::decoder::load_audio_file;
use crate::dsp::k_weighting::KWeightingFilter;

/// Samples at or above this are treated as clipped (just under 16-bit full scale)
const CLIP_LEVEL: f32 = 0.99997;
/// Consecutive clipped samples that count as a clip event
const CLIP_RUN: usize = 3;
/// Below this a window counts as silent (-90 dBFS RMS)
const SILENCE_POWER: f64 = 1e-9;
/// Oversampling factor and taps per phase of the true-peak interpolator
const TP_FACTOR: usize = 4;
const TP_TAPS: usize = 16;

/// What the master should hit, for pass/warn judgements
#[derive(Clone, Copy, Debug, Default)]
pub struct QcTargets {
    pub target_lufs: Option<f32>,
    pub ceiling_dbtp: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum QcStatus {
    Pass,
    Warn,
    Fail,
}

impl QcStatus {
    pub fn label(self) -> &'static str {
        match self {
            QcStatus::Pass => "PASS",
            QcStatus::Warn => "WARN",
            QcStatus::Fail => "FAIL",
        }
    }
}

#[derive(Clone, Debug)]
pub struct QcCheck {
    pub name: &'static str,
    pub status: QcStatus,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct QcReport {
    /// Where the measured audio came from (file name, or a description)
    pub source: String,
    /// Format description (e.g. "FLAC 24-bit")
    pub format: String,
    pub sample_rate: u32,
    pub duration_s: f64,
    pub integrated_lufs: f32,
    pub loudness_range_lu: f32,
    pub max_momentary_lufs: f32,
    pub max_short_term_lufs: f32,
    pub true_peak_dbtp: [f32; 2],
    pub sample_peak_dbfs: [f32; 2],
    /// Inter-sample peaks above 0 dBTP (counted on the 4x oversampled signal)
    pub true_peak_overs: usize,
    pub clipped_samples: usize,
    pub clip_events: usize,
    pub correlation: f32,
    /// Lowest correlation over 400 ms windows (silent windows skipped)
    pub min_correlation: f32,
    /// Share of non-silent 400 ms windows with negative correlation
    pub negative_correlation_share: f32,
    /// Loudness change when folded to mono (L+R)/2, in dB
    pub mono_loudness_change_db: f32,
    pub dc_offset_dbfs: [f32; 2],
    pub leading_silence_s: f64,
    pub trailing_silence_s: f64,
    pub checks: Vec<QcCheck>,
}

fn db(linear: f32) -> f32 {
    20.0 * linear.max(1e-10).log10()
}

fn power_to_lufs(p: f64) -> f32 {
    if p <= 1e-12 {
        -70.0
    } else {
        (-0.691 + 10.0 * p.log10()) as f32
    }
}

/// ITU-R BS.1770 gated mean over 400 ms blocks (absolute -70 LUFS, relative -10 LU)
fn gated_loudness(blocks: &[f64]) -> f32 {
    let abs = 10f64.powf((-70.0 + 0.691) / 10.0);
    let above: Vec<f64> = blocks.iter().copied().filter(|&p| p >= abs).collect();
    if above.is_empty() {
        return -70.0;
    }
    let ungated = above.iter().sum::<f64>() / above.len() as f64;
    let rel = ungated * 10f64.powf(-10.0 / 10.0);
    let gated: Vec<f64> = above.into_iter().filter(|&p| p >= rel).collect();
    if gated.is_empty() {
        return -70.0;
    }
    power_to_lufs(gated.iter().sum::<f64>() / gated.len() as f64)
}

/// EBU Tech 3342 loudness range from short-term (3 s) powers: 10th to 95th percentile of the
/// blocks above -70 LUFS and above 20 LU below their mean
fn loudness_range(short_term: &[f64]) -> f32 {
    let abs = 10f64.powf((-70.0 + 0.691) / 10.0);
    let above: Vec<f64> = short_term.iter().copied().filter(|&p| p >= abs).collect();
    if above.len() < 2 {
        return 0.0;
    }
    let mean = above.iter().sum::<f64>() / above.len() as f64;
    let rel = mean * 10f64.powf(-20.0 / 10.0);
    let mut lufs: Vec<f32> = above.into_iter().filter(|&p| p >= rel).map(power_to_lufs).collect();
    if lufs.len() < 2 {
        return 0.0;
    }
    lufs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pick = |q: f32| lufs[((lufs.len() - 1) as f32 * q).round() as usize];
    pick(0.95) - pick(0.10)
}

/// 4x polyphase windowed-sinc interpolator for true-peak measurement (BS.1770 Annex 2 style)
struct TruePeak {
    phases: Vec<[f32; TP_TAPS]>,
}

impl TruePeak {
    fn new() -> Self {
        let half = TP_TAPS as f32 / 2.0;
        let phases = (0..TP_FACTOR)
            .map(|p| {
                let frac = p as f32 / TP_FACTOR as f32;
                let mut taps = [0.0f32; TP_TAPS];
                for (j, t) in taps.iter_mut().enumerate() {
                    // Tap j weighs input sample (n - half + 1 + j) for output time n + frac
                    let x = j as f32 - (half - 1.0) - frac;
                    let sinc = if x.abs() < 1e-6 { 1.0 } else { (PI * x).sin() / (PI * x) };
                    let w = 0.5 * (1.0 + (PI * x / (half + 1.0)).cos());
                    *t = sinc * w;
                }
                // Unity gain at DC, so a constant reads as itself
                let sum: f32 = taps.iter().sum();
                taps.iter_mut().for_each(|t| *t /= sum);
                taps
            })
            .collect();
        Self { phases }
    }

    /// Highest absolute interpolated value, and how many interpolated points exceed 1.0
    fn measure(&self, x: &[f32]) -> (f32, usize) {
        let half = TP_TAPS / 2;
        // Zero padding so every output point reads a full window: taps j cover x[n - half + 1 + j]
        let mut padded = vec![0.0f32; x.len() + TP_TAPS];
        padded[half - 1..half - 1 + x.len()].copy_from_slice(x);
        let mut peak = 0.0f32;
        let mut overs = 0;
        for (n, &v) in x.iter().enumerate() {
            peak = peak.max(v.abs());
            overs += (v.abs() > 1.0) as usize;
            let window = &padded[n..n + TP_TAPS];
            for phase in &self.phases[1..] {
                let y: f32 = phase.iter().zip(window).map(|(c, s)| c * s).sum();
                peak = peak.max(y.abs());
                overs += (y.abs() > 1.0) as usize;
            }
        }
        (peak, overs)
    }
}

/// Measure a decoded master
pub fn analyze_buffer(buf: &AudioBuffer, source: &str, format: &str, targets: QcTargets) -> QcReport {
    let (left, right) = buf.to_stereo();
    let n = left.len();
    let sr = buf.sample_rate.max(1);

    // Loudness: K-weighted power per 100 ms block, stereo and mono fold-down
    let block = (sr as usize / 10).max(1);
    let mut kw = KWeightingFilter::new(sr as f32);
    let mut kw_mono = KWeightingFilter::new(sr as f32);
    let mut sub = Vec::with_capacity(n / block + 1);
    let mut sub_mono = Vec::with_capacity(n / block + 1);
    let (mut acc, mut acc_mono) = (0.0f64, 0.0f64);
    for i in 0..n {
        let (kl, kr) = kw.process_sample(left[i], right[i]);
        let mid = 0.5 * (left[i] + right[i]);
        let (km, _) = kw_mono.process_sample(mid, mid);
        acc += (kl * kl + kr * kr) as f64;
        acc_mono += 2.0 * (km * km) as f64;
        if (i + 1) % block == 0 {
            sub.push(acc / block as f64);
            sub_mono.push(acc_mono / block as f64);
            acc = 0.0;
            acc_mono = 0.0;
        }
    }
    let window = |blocks: &[f64], len: usize| -> Vec<f64> {
        if blocks.len() < len {
            return Vec::new();
        }
        blocks.windows(len).map(|w| w.iter().sum::<f64>() / len as f64).collect()
    };
    let momentary = window(&sub, 4);
    let momentary_mono = window(&sub_mono, 4);
    let short_term = window(&sub, 30);
    let integrated = gated_loudness(&momentary);
    let integrated_mono = gated_loudness(&momentary_mono);
    let max_of = |v: &[f64]| v.iter().copied().fold(None, |m: Option<f64>, p| Some(m.map_or(p, |m| m.max(p))));
    let max_momentary = max_of(&momentary).map_or(-70.0, power_to_lufs);
    let max_short_term = max_of(&short_term).map_or(-70.0, power_to_lufs);
    let lra = loudness_range(&short_term);

    // Peaks and clipping
    let tp = TruePeak::new();
    let mut true_peak = [0.0f32; 2];
    let mut sample_peak = [0.0f32; 2];
    let mut overs = 0;
    let (mut clipped, mut clip_events) = (0, 0);
    for (c, ch) in [&left, &right].into_iter().enumerate() {
        let (p, o) = tp.measure(ch);
        true_peak[c] = p;
        overs += o;
        sample_peak[c] = ch.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let mut run = 0;
        for &v in ch.iter() {
            if v.abs() >= CLIP_LEVEL {
                clipped += 1;
                run += 1;
                if run == CLIP_RUN {
                    clip_events += 1;
                }
            } else {
                run = 0;
            }
        }
    }

    // Phase: whole-track and 400 ms windows
    let corr = |l: &[f32], r: &[f32]| -> Option<f32> {
        let (mut lr, mut ll, mut rr) = (0.0f64, 0.0f64, 0.0f64);
        for (&a, &b) in l.iter().zip(r) {
            lr += (a * b) as f64;
            ll += (a * a) as f64;
            rr += (b * b) as f64;
        }
        let len = l.len().max(1) as f64;
        if (ll + rr) / len < SILENCE_POWER {
            return None;
        }
        Some(if ll * rr > 0.0 { (lr / (ll * rr).sqrt()) as f32 } else { 1.0 })
    };
    let correlation = corr(&left, &right).unwrap_or(1.0);
    let win = (sr as usize * 2 / 5).max(1);
    let windows: Vec<f32> = left.chunks(win).zip(right.chunks(win)).filter_map(|(l, r)| corr(l, r)).collect();
    let min_correlation = windows.iter().copied().fold(1.0f32, f32::min);
    let negative_share = if windows.is_empty() { 0.0 } else { windows.iter().filter(|&&c| c < 0.0).count() as f32 / windows.len() as f32 };

    let dc = [left.iter().map(|&v| v as f64).sum::<f64>(), right.iter().map(|&v| v as f64).sum::<f64>()]
        .map(|s| db((s / n.max(1) as f64).abs() as f32));

    // Silence at the ends: below -90 dBFS
    let silent = |v: f32| v.abs() < 3.2e-5;
    let lead = (0..n).find(|&i| !silent(left[i]) || !silent(right[i])).unwrap_or(n);
    let trail = (0..n).rev().find(|&i| !silent(left[i]) || !silent(right[i])).map_or(n, |i| n - 1 - i);

    let mut report = QcReport {
        source: source.to_string(),
        format: format.to_string(),
        sample_rate: sr,
        duration_s: n as f64 / sr as f64,
        integrated_lufs: integrated,
        loudness_range_lu: lra,
        max_momentary_lufs: max_momentary,
        max_short_term_lufs: max_short_term,
        true_peak_dbtp: true_peak.map(db),
        sample_peak_dbfs: sample_peak.map(db),
        true_peak_overs: overs,
        clipped_samples: clipped,
        clip_events,
        correlation,
        min_correlation,
        negative_correlation_share: negative_share,
        mono_loudness_change_db: if integrated > -70.0 { integrated_mono - integrated } else { 0.0 },
        dc_offset_dbfs: dc,
        leading_silence_s: lead as f64 / sr as f64,
        trailing_silence_s: trail as f64 / sr as f64,
        checks: Vec::new(),
    };
    report.checks = judge(&report, targets);
    report
}

fn judge(r: &QcReport, targets: QcTargets) -> Vec<QcCheck> {
    let mut checks = Vec::new();
    let mut add = |name, status, detail: String| checks.push(QcCheck { name, status, detail });

    if r.integrated_lufs <= -70.0 {
        add("Loudness", QcStatus::Fail, "the file is silent".to_string());
    } else if let Some(t) = targets.target_lufs {
        let diff = r.integrated_lufs - t;
        let status = if diff.abs() <= 1.0 { QcStatus::Pass } else { QcStatus::Warn };
        let hint = if diff > 1.0 {
            " (streaming services will turn it down)"
        } else if diff < -1.0 {
            " (quieter than the target)"
        } else {
            ""
        };
        add("Loudness", status, format!("{:.1} LUFS integrated, target {:.1} ({:+.1} LU){}", r.integrated_lufs, t, diff, hint));
    } else {
        add("Loudness", QcStatus::Pass, format!("{:.1} LUFS integrated", r.integrated_lufs));
    }

    let tp = r.true_peak_dbtp[0].max(r.true_peak_dbtp[1]);
    let tp_status = if tp > 0.0 {
        QcStatus::Fail
    } else if targets.ceiling_dbtp.is_some_and(|c| tp > c + 0.1) {
        QcStatus::Warn
    } else {
        QcStatus::Pass
    };
    let ceiling = targets.ceiling_dbtp.map(|c| format!(", ceiling {:.1} dBTP", c)).unwrap_or_default();
    let overs = if r.true_peak_overs > 0 { format!(", {} inter-sample overs", r.true_peak_overs) } else { String::new() };
    add("True peak", tp_status, format!("{:.2} dBTP{}{}", tp, ceiling, overs));

    let clip_status = if r.clip_events > 0 { QcStatus::Fail } else if r.clipped_samples > 0 { QcStatus::Warn } else { QcStatus::Pass };
    add(
        "Clipping",
        clip_status,
        if r.clipped_samples == 0 {
            "no samples at full scale".to_string()
        } else {
            format!("{} samples at full scale, {} runs of {}+", r.clipped_samples, r.clip_events, CLIP_RUN)
        },
    );

    let phase_status = if r.correlation < 0.0 {
        QcStatus::Fail
    } else if r.correlation < 0.3 || r.min_correlation < -0.3 || r.negative_correlation_share > 0.05 {
        QcStatus::Warn
    } else {
        QcStatus::Pass
    };
    add(
        "Phase",
        phase_status,
        format!(
            "correlation {:+.2} overall, {:+.2} at worst, negative {:.0}% of the time",
            r.correlation,
            r.min_correlation,
            r.negative_correlation_share * 100.0
        ),
    );

    let mono_status = if r.mono_loudness_change_db < -3.5 { QcStatus::Warn } else { QcStatus::Pass };
    add("Mono compatibility", mono_status, format!("{:+.1} dB when folded to mono", r.mono_loudness_change_db));

    let dc = r.dc_offset_dbfs[0].max(r.dc_offset_dbfs[1]);
    add("DC offset", if dc > -60.0 { QcStatus::Warn } else { QcStatus::Pass }, format!("{:.0} dBFS", dc));

    let edges = if r.leading_silence_s > 2.0 || r.trailing_silence_s > 10.0 { QcStatus::Warn } else { QcStatus::Pass };
    add("Silence", edges, format!("{:.2} s at the start, {:.2} s at the end", r.leading_silence_s, r.trailing_silence_s));
    checks
}

/// Decode an exported file and measure it
pub fn analyze_file(path: &Path, format: &str, targets: QcTargets) -> Result<QcReport, String> {
    let buf = load_audio_file(path).map_err(|e| format!("QC could not read {}: {}", path.display(), e))?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Ok(analyze_buffer(&buf, &name, format, targets))
}

impl QcReport {
    /// Worst status over all checks
    pub fn overall(&self) -> QcStatus {
        self.checks.iter().map(|c| c.status).max().unwrap_or(QcStatus::Pass)
    }

    pub fn summary(&self) -> String {
        let flagged: Vec<&str> = self.checks.iter().filter(|c| c.status != QcStatus::Pass).map(|c| c.name).collect();
        match self.overall() {
            QcStatus::Pass => format!("QC passed ({:.1} LUFS, {:.1} dBTP)", self.integrated_lufs, self.true_peak_dbtp[0].max(self.true_peak_dbtp[1])),
            s => format!("QC {}: {}", s.label().to_lowercase(), flagged.join(", ")),
        }
    }

    pub fn to_text(&self) -> String {
        let mut t = String::new();
        let mins = (self.duration_s / 60.0).floor();
        let _ = writeln!(t, "QC report: {}", self.source);
        let _ = writeln!(t, "{}, {} Hz, {}:{:05.2}", self.format, self.sample_rate, mins, self.duration_s - mins * 60.0);
        let _ = writeln!(t, "Overall: {}\n", self.overall().label());
        for c in &self.checks {
            let _ = writeln!(t, "  [{}] {:<19} {}", c.status.label(), c.name, c.detail);
        }
        let _ = writeln!(t, "\nLoudness");
        let _ = writeln!(t, "  Integrated        {:7.1} LUFS", self.integrated_lufs);
        let _ = writeln!(t, "  Loudness range    {:7.1} LU", self.loudness_range_lu);
        let _ = writeln!(t, "  Max short-term    {:7.1} LUFS", self.max_short_term_lufs);
        let _ = writeln!(t, "  Max momentary     {:7.1} LUFS", self.max_momentary_lufs);
        let _ = writeln!(t, "Peaks (L / R)");
        let _ = writeln!(t, "  True peak         {:7.2} / {:.2} dBTP", self.true_peak_dbtp[0], self.true_peak_dbtp[1]);
        let _ = writeln!(t, "  Sample peak       {:7.2} / {:.2} dBFS", self.sample_peak_dbfs[0], self.sample_peak_dbfs[1]);
        let _ = writeln!(t, "  DC offset         {:7.0} / {:.0} dBFS", self.dc_offset_dbfs[0], self.dc_offset_dbfs[1]);
        let _ = writeln!(t, "Stereo");
        let _ = writeln!(t, "  Correlation       {:+7.2} (worst 400 ms {:+.2})", self.correlation, self.min_correlation);
        let _ = writeln!(t, "  Mono fold-down    {:+7.1} dB", self.mono_loudness_change_db);
        t
    }

    /// Path of the report written next to an export: `master.wav` -> `master.qc.txt`
    pub fn report_path(export: &Path) -> PathBuf {
        let stem = export.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "export".to_string());
        export.with_file_name(format!("{}.qc.txt", stem))
    }

    pub fn write_beside(&self, export: &Path) -> Result<PathBuf, String> {
        let path = Self::report_path(export);
        std::fs::write(&path, self.to_text()).map_err(|e| format!("Could not write QC report: {}", e))?;
        Ok(path)
    }
}
