//! Match reference (AI): set the suite so the track sounds like a reference.
//!
//! The style model only scores how close two clips sound; the suite's own DSP does the processing.
//! A separable CMA-ES searches a `SearchSpace`: each candidate renders a few excerpts of the track
//! through a copy of the suite and is scored by its embedding's distance to the reference's.
//! Embeddings ignore gain, so the search can't get closer by just going louder.

use std::cell::Cell;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::style_space::{ParamChange, SearchSpace};
use super::{EbuR128Meter, MasteringSuite};
use crate::audio::buffer::AudioBuffer;
use crate::audio::ml_separator::{StemBus, StemSet};
use crate::audio::ml_style::{StyleEmbedding, StyleFeatures, StyleFrontEnd, StyleModel, MIN_SECONDS};

pub const CANCELLED: &str = "Cancelled";

const EXCERPT_SECONDS: usize = 4;
/// Audio run through the chain before an excerpt, so envelopes have settled
const SETTLE_SECONDS: f32 = 0.25;
const BLOCK: usize = 1024;
/// Windows this close to the loudest (power ratio, -3 dB) count as the same section
const SECTION_RATIO: f64 = 0.5;

// -------------------------------------------------------------------------------------------------
// Optimiser
// -------------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct CmaOptions {
    pub generations: usize,
    pub sigma: f64,
    /// Stop when the best score improves by less than `stall_tolerance` (relative) over
    /// `stall_generations`
    pub stall_generations: usize,
    pub stall_tolerance: f64,
    pub time_limit: Option<Duration>,
    pub seed: u64,
}

impl Default for CmaOptions {
    fn default() -> Self {
        Self {
            generations: 30,
            sigma: 0.2,
            stall_generations: 5,
            stall_tolerance: 0.001,
            time_limit: Some(Duration::from_secs(90)),
            seed: 0x5A0E_C0DE,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CmaResult {
    pub best: Vec<f32>,
    pub best_score: f32,
    pub generations: usize,
    pub evaluations: usize,
}

/// SplitMix64, so a seed gives the same search on every platform
struct Rng {
    state: u64,
    spare: Option<f64>,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed, spare: None }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal (Box-Muller)
    fn normal(&mut self) -> f64 {
        if let Some(v) = self.spare.take() {
            return v;
        }
        let radius = (-2.0 * (1.0 - self.uniform()).ln()).sqrt();
        let angle = 2.0 * std::f64::consts::PI * self.uniform();
        self.spare = Some(radius * angle.sin());
        radius * angle.cos()
    }
}

/// Fold a coordinate back into [0, 1], so samples past a bound aren't all stuck on it
fn mirror(x: f64) -> f64 {
    let m = x.rem_euclid(2.0);
    if m > 1.0 { 2.0 - m } else { m }
}

pub fn population_size(dimensions: usize) -> usize {
    4 + (3.0 * (dimensions as f64).ln()).floor() as usize
}

/// Minimise a function over [0, 1]^n with separable CMA-ES (Ros & Hansen, 2008). `evaluate` scores
/// a whole generation; `after_generation` gets its number and the best score so far and returns
/// false to stop.
pub fn minimize(
    x0: &[f32],
    options: &CmaOptions,
    mut evaluate: impl FnMut(&[Vec<f32>]) -> Result<Vec<f32>, String>,
    mut after_generation: impl FnMut(usize, f32) -> bool,
) -> Result<CmaResult, String> {
    let n = x0.len();
    if n == 0 {
        return Err("Nothing to optimise".to_string());
    }
    let dim = n as f64;
    let lambda = population_size(n);
    let mu = lambda / 2;
    let raw: Vec<f64> = (0..mu).map(|i| (mu as f64 + 0.5).ln() - ((i + 1) as f64).ln()).collect();
    let total: f64 = raw.iter().sum();
    let weights: Vec<f64> = raw.iter().map(|w| w / total).collect();
    let mu_eff = 1.0 / weights.iter().map(|w| w * w).sum::<f64>();

    let c_sigma = (mu_eff + 2.0) / (dim + mu_eff + 5.0);
    let d_sigma = 1.0 + 2.0 * (((mu_eff - 1.0) / (dim + 1.0)).sqrt() - 1.0).max(0.0) + c_sigma;
    let c_c = 4.0 / (dim + 4.0);
    // The diagonal has only n terms to learn, so it adapts (n + 2) / 3 times faster
    let speed = (dim + 2.0) / 3.0;
    let c_1 = (2.0 / ((dim + 1.3).powi(2) + mu_eff) * speed).min(1.0);
    let c_mu = (2.0 * (mu_eff - 2.0 + 1.0 / mu_eff) / ((dim + 2.0).powi(2) + mu_eff) * speed).min(1.0 - c_1);
    let chi_n = dim.sqrt() * (1.0 - 1.0 / (4.0 * dim) + 1.0 / (21.0 * dim * dim));

    let mut rng = Rng::new(options.seed);
    let mut mean: Vec<f64> = x0.iter().map(|&v| v as f64).collect();
    let mut sigma = options.sigma;
    let mut variance = vec![1.0f64; n];
    let mut path_sigma = vec![0.0f64; n];
    let mut path_c = vec![0.0f64; n];

    let started = Instant::now();
    let mut best = CmaResult { best: x0.to_vec(), best_score: f32::INFINITY, generations: 0, evaluations: 0 };
    let mut history: Vec<f32> = Vec::with_capacity(options.generations);

    for generation in 1..=options.generations {
        let samples: Vec<Vec<f64>> = (0..lambda)
            .map(|_| (0..n).map(|i| mirror(mean[i] + sigma * variance[i].sqrt() * rng.normal())).collect())
            .collect();
        let candidates: Vec<Vec<f32>> = samples.iter().map(|s| s.iter().map(|&v| v as f32).collect()).collect();
        let scores = evaluate(&candidates)?;
        if scores.len() != lambda {
            return Err("Optimiser got the wrong number of scores".to_string());
        }
        best.evaluations += lambda;
        best.generations = generation;

        let mut order: Vec<usize> = (0..lambda).collect();
        order.sort_by(|&a, &b| scores[a].total_cmp(&scores[b]));
        if scores[order[0]] < best.best_score {
            best.best_score = scores[order[0]];
            best.best = candidates[order[0]].clone();
        }

        let old_mean = std::mem::take(&mut mean);
        mean = (0..n).map(|i| (0..mu).map(|k| weights[k] * samples[order[k]][i]).sum()).collect();
        let step: Vec<f64> = (0..n).map(|i| (mean[i] - old_mean[i]) / sigma).collect();

        let norm_factor = (c_sigma * (2.0 - c_sigma) * mu_eff).sqrt();
        for i in 0..n {
            path_sigma[i] = (1.0 - c_sigma) * path_sigma[i] + norm_factor * step[i] / variance[i].sqrt();
        }
        let path_norm = path_sigma.iter().map(|v| v * v).sum::<f64>().sqrt();
        let fade = 1.0 - (1.0 - c_sigma).powi(2 * generation as i32);
        let stalled_step = path_norm / fade.sqrt() / chi_n >= 1.4 + 2.0 / (dim + 1.0);
        let h_sigma = if stalled_step { 0.0 } else { 1.0 };

        let c_factor = (c_c * (2.0 - c_c) * mu_eff).sqrt();
        for i in 0..n {
            path_c[i] = (1.0 - c_c) * path_c[i] + h_sigma * c_factor * step[i];
            let elite: f64 = (0..mu).map(|k| weights[k] * ((samples[order[k]][i] - old_mean[i]) / sigma).powi(2)).sum();
            let correction = (1.0 - h_sigma) * c_c * (2.0 - c_c) * variance[i];
            variance[i] = (1.0 - c_1 - c_mu) * variance[i] + c_1 * (path_c[i].powi(2) + correction) + c_mu * elite;
            variance[i] = variance[i].max(1e-12);
        }
        sigma *= ((c_sigma / d_sigma) * (path_norm / chi_n - 1.0)).exp();

        history.push(best.best_score);
        if !after_generation(generation, best.best_score) {
            break;
        }
        if let Some(limit) = options.time_limit {
            if started.elapsed() >= limit {
                break;
            }
        }
        if let Some(&earlier) = history.len().checked_sub(options.stall_generations + 1).map(|i| &history[i]) {
            if ((earlier - best.best_score) as f64) < options.stall_tolerance * earlier.abs() as f64 {
                break;
            }
        }
    }
    Ok(best)
}

// -------------------------------------------------------------------------------------------------
// Excerpts and rendering
// -------------------------------------------------------------------------------------------------

/// Start frames of the windows a match listens to: the loudest, the median and the start of the
/// loudest section, by momentary loudness. Tracks up to one window long give a single window.
pub fn excerpt_starts(track: &AudioBuffer) -> Vec<usize> {
    let sr = track.sample_rate as usize;
    let length = EXCERPT_SECONDS * sr;
    let frames = track.num_frames();
    let hop = (sr / 10).max(1);
    if frames <= length + hop {
        return vec![0];
    }
    let (left, right) = track.to_stereo();
    let mut meter = EbuR128Meter::new(track.sample_rate as f32);
    let power: Vec<f64> = left
        .chunks_exact(hop)
        .zip(right.chunks_exact(hop))
        .map(|(l, r)| {
            meter.process_block(l, r);
            10f64.powf(meter.momentary_lufs() as f64 / 10.0)
        })
        .collect();
    let span = length / hop;
    let windows: Vec<f64> = power.windows(span).map(|w| w.iter().sum::<f64>() / span as f64).collect();

    let mut order: Vec<usize> = (0..windows.len()).collect();
    order.sort_by(|&a, &b| windows[a].total_cmp(&windows[b]).then(a.cmp(&b)));
    let loudest = order[order.len() - 1];
    let median = order[order.len() / 2];
    let mut section = loudest;
    while section > 0 && windows[section - 1] >= windows[loudest] * SECTION_RATIO {
        section -= 1;
    }
    let mut starts: Vec<usize> = [loudest, median, section].iter().map(|&w| (w * hop).min(frames - length)).collect();
    starts.sort_unstable();
    starts.dedup();
    starts
}

/// A window of the chain's input, with audio in front of it for the chain to settle on
#[derive(Clone)]
pub struct Excerpt {
    left: Vec<f32>,
    right: Vec<f32>,
    /// Frames to drop from the chain's output
    skip: usize,
    length: usize,
}

impl Excerpt {
    /// Copy `length` frames at `start`, plus what `latency` needs to bring them out of the chain
    /// aligned. With separated stems the input is their mix, as in an export.
    fn new(track: &AudioBuffer, stems: Option<(&StemSet, &mut StemBus)>, start: usize, length: usize, latency: usize) -> Self {
        let settle = (track.sample_rate as f32 * SETTLE_SECONDS) as usize;
        let feed = settle + length + latency;
        let mut left = Vec::with_capacity(feed);
        let mut right = Vec::with_capacity(feed);
        let mut stems = stems;
        if let Some((_, bus)) = stems.as_mut() {
            bus.reset();
        }
        let first = start as isize - settle as isize;
        for frame in first..first + feed as isize {
            let (l, r) = match (usize::try_from(frame).ok().filter(|&f| f < track.num_frames()), stems.as_mut()) {
                (None, _) => (0.0, 0.0),
                (Some(f), Some((set, bus))) => bus.mix(set.frames_at_index(f)),
                (Some(f), None) => track.get_stereo_frame(f),
            };
            left.push(l);
            right.push(r);
        }
        Self { left, right, skip: settle + latency, length }
    }

    /// Run the excerpt through `suite`, which should be freshly reset; returns the window itself
    pub fn render(&self, suite: &mut MasteringSuite) -> (Vec<f32>, Vec<f32>) {
        let mut left = self.left.clone();
        let mut right = self.right.clone();
        for (l, r) in left.chunks_mut(BLOCK).zip(right.chunks_mut(BLOCK)) {
            suite.process_stereo(l, r);
        }
        let range = self.skip..self.skip + self.length;
        (left[range.clone()].to_vec(), right[range].to_vec())
    }
}

/// The excerpts of `track` for a chain with `latency` frames of delay
pub fn prepare_excerpts(track: &AudioBuffer, stems: Option<&StemSet>, base: &MasteringSuite, latency: usize) -> Vec<Excerpt> {
    let length = (EXCERPT_SECONDS * track.sample_rate as usize).min(track.num_frames());
    let mut bus = stems.map(|_| {
        let mut bus = StemBus::new(track.sample_rate as f32);
        bus.configure(base);
        bus
    });
    excerpt_starts(track)
        .into_iter()
        .map(|start| Excerpt::new(track, stems.zip(bus.as_mut()), start, length, latency))
        .collect()
}

/// Render one candidate's excerpts ready for the model. `suite` already holds its settings.
pub fn render_candidate(suite: &mut MasteringSuite, excerpts: &[Excerpt], front_end: &StyleFrontEnd) -> Result<Vec<StyleFeatures>, String> {
    let sample_rate = suite.sample_rate as u32;
    excerpts
        .iter()
        .map(|excerpt| {
            suite.reset_state();
            let (left, right) = excerpt.render(suite);
            front_end.features(&left, &right, sample_rate)
        })
        .collect()
}

/// The reference's style: its 4 s windows, averaged
fn reference_style(reference: &AudioBuffer, model: &mut StyleModel) -> Result<StyleEmbedding, String> {
    let (left, right) = reference.to_stereo();
    let length = EXCERPT_SECONDS * reference.sample_rate as usize;
    let mut features = Vec::new();
    for (l, r) in left.chunks(length).zip(right.chunks(length)) {
        // Only a short tail can fail, and the whole clip was checked first
        if let Ok(f) = model.front_end().features(l, r, reference.sample_rate) {
            features.push(f);
        }
    }
    StyleEmbedding::average(&model.embed_features(&features)?).ok_or_else(|| "Reference is too short to compare".to_string())
}

/// Mean distance of each candidate, from its excerpts' embeddings in order
fn mean_distances(embeddings: &[StyleEmbedding], per_candidate: usize, target: &StyleEmbedding) -> Vec<f32> {
    embeddings
        .chunks(per_candidate)
        .map(|c| {
            let mean = c.iter().map(|e| e.distance(target)).sum::<f32>() / c.len() as f32;
            if mean.is_finite() { mean } else { f32::MAX }
        })
        .collect()
}

type RenderedCandidate = Result<Vec<StyleFeatures>, String>;

/// Render a generation on one suite copy per thread, in the candidates' order
fn render_generation(
    workers: &mut [MasteringSuite],
    space: &SearchSpace,
    candidates: &[Vec<f32>],
    excerpts: &[Excerpt],
    front_end: &StyleFrontEnd,
) -> Result<Vec<StyleFeatures>, String> {
    let next = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<RenderedCandidate>>> = Mutex::new((0..candidates.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for suite in workers.iter_mut() {
            let (next, slots) = (&next, &slots);
            scope.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(candidate) = candidates.get(i) else { break };
                space.decode(candidate, suite);
                let rendered = render_candidate(suite, excerpts, front_end);
                slots.lock()[i] = Some(rendered);
            });
        }
    });
    let mut features = Vec::with_capacity(candidates.len() * excerpts.len());
    for slot in slots.into_inner() {
        features.extend(slot.ok_or("A render thread failed")??);
    }
    Ok(features)
}

// -------------------------------------------------------------------------------------------------
// The match
// -------------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct MatchOptions {
    /// Leave switched-off modules alone; otherwise a match may switch them on
    pub only_enabled: bool,
    /// Maximizer ceiling to hold through the search
    pub ceiling_db: Option<f32>,
    /// Cost of squared movement from the starting settings, as a fraction of the starting style
    /// distance (so it means the same for close and distant references). It keeps the match from
    /// making big changes for tiny gains.
    pub regularisation: f32,
    pub search: CmaOptions,
    /// Checked once per generation
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self { only_enabled: true, ceiling_db: None, regularisation: 0.05, search: CmaOptions::default(), cancel: None }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MatchProgress {
    pub generation: usize,
    pub generations: usize,
    /// How much closer the best candidate so far is, in percent of the starting distance
    pub closer_percent: f32,
}

#[derive(Clone, Debug)]
pub struct MatchResult {
    pub space: SearchSpace,
    pub best: Vec<f32>,
    pub start_distance: f32,
    pub best_distance: f32,
    pub generations: usize,
    pub evaluations: usize,
    /// Empty when no candidate beat the starting settings
    pub changes: Vec<ParamChange>,
}

impl MatchResult {
    pub fn improved(&self) -> bool {
        !self.changes.is_empty()
    }

    pub fn improvement_percent(&self) -> f32 {
        closer_percent(self.start_distance, self.best_distance)
    }

    pub fn to_text(&self) -> String {
        let mut text = format!(
            "{:.0}% closer to the reference (style distance {:.3} to {:.3})\n{} candidates over {} generations\n\n",
            self.improvement_percent(),
            self.start_distance,
            self.best_distance,
            self.evaluations,
            self.generations
        );
        for c in &self.changes {
            let _ = writeln!(text, "{}: {} to {}", c.name, c.before, c.after);
        }
        text
    }
}

fn closer_percent(start: f32, now: f32) -> f32 {
    if start > 0.0 { (1.0 - now / start) * 100.0 } else { 0.0 }
}

/// Find the suite settings that bring `track` closest to `reference`. `stems` are the track's
/// separated stems, if any. Blocking and slow: run it on a worker thread. `suite` isn't changed;
/// apply the result with `result.space.decode(&result.best, ...)`. Errors with `CANCELLED` if
/// `options.cancel` gets set.
pub fn match_to_reference(
    track: &AudioBuffer,
    reference: &AudioBuffer,
    suite: &MasteringSuite,
    model: &mut StyleModel,
    stems: Option<&StemSet>,
    options: &MatchOptions,
    progress: impl Fn(MatchProgress),
) -> Result<MatchResult, String> {
    if track.duration_seconds() < MIN_SECONDS {
        return Err("Track is too short to compare".to_string());
    }
    if reference.duration_seconds() < MIN_SECONDS {
        return Err("Reference is too short to compare".to_string());
    }
    let cancelled = || options.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed));
    let mut base = suite.clone();
    base.set_sample_rate(track.sample_rate as f32);
    let stems = stems.filter(|s| s.num_frames == track.num_frames() && s.sample_rate == track.sample_rate && base.uses_stems());
    base.external_stems = stems.is_some();

    let mut space = SearchSpace::for_suite(&base, options.only_enabled);
    if let Some(ceiling) = options.ceiling_db {
        space = space.with_ceiling(ceiling);
    }
    if space.is_empty() {
        return Err("No module to adjust: switch on the Equalizer, Vintage Compressor, Stereo Imager, Harmonic Exciter or Maximizer".to_string());
    }
    let x0 = space.encode(&base);
    let mut working = base.clone();
    space.decode(&x0, &mut working);

    let front_end = StyleFrontEnd::default();
    let target = reference_style(reference, model)?;

    // The starting score is the suite as it stands, not its nearest point on the search grid
    let excerpts = prepare_excerpts(track, stems, &base, working.latency_samples());
    let start = render_candidate(&mut base.clone(), &excerpts, &front_end)?;
    let start_distance = mean_distances(&model.embed_features(&start)?, excerpts.len(), &target)[0];

    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
    let mut workers = vec![working; threads.min(population_size(space.len()))];
    let penalty_scale = options.regularisation * start_distance;
    let penalty = |x: &[f32]| penalty_scale * x.iter().zip(&x0).map(|(a, b)| (a - b) * (a - b)).sum::<f32>();

    // The optimiser's score includes the penalty; reports are about the distance alone
    let lowest_score = Cell::new(f32::INFINITY);
    let best_distance = Cell::new(start_distance);
    let search = minimize(
        &x0,
        &options.search,
        |candidates| {
            let features = render_generation(&mut workers, &space, candidates, &excerpts, &front_end)?;
            let embeddings = model.embed_features(&features)?;
            let distances = mean_distances(&embeddings, excerpts.len(), &target);
            let scores: Vec<f32> = distances.iter().zip(candidates).map(|(d, x)| d + penalty(x)).collect();
            for (&score, &distance) in scores.iter().zip(&distances) {
                if score < lowest_score.get() {
                    lowest_score.set(score);
                    if score < start_distance {
                        best_distance.set(distance);
                    }
                }
            }
            Ok(scores)
        },
        |generation, _| {
            progress(MatchProgress {
                generation,
                generations: options.search.generations,
                closer_percent: closer_percent(start_distance, best_distance.get()),
            });
            !cancelled()
        },
    )?;
    if cancelled() {
        return Err(CANCELLED.to_string());
    }

    let improved = search.best_score < start_distance;
    Ok(MatchResult {
        changes: if improved { space.changes(&search.best, &base) } else { Vec::new() },
        best: search.best,
        space,
        start_distance,
        best_distance: best_distance.get(),
        generations: search.generations,
        evaluations: search.evaluations,
    })
}
