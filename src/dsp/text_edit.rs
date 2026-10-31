//! Text requests ("warmer, less harsh, wider"): a small language model turns them into a few small
//! moves of the suite's parameters, as a starting point for your ears or Match reference (AI).
//!
//! The prompt follows LLM2Fx (Sony AI, WASPAA 2025): the track's measurements, the parameters'
//! definitions and worked examples. Small models are unreliable, so a generated grammar keeps the
//! answer to known parameter names, and every move is limited again here before it's applied.

use serde::Deserialize;
use std::fmt::Write as _;

use super::module::{ModuleKind, ParamInfo};
use super::style_space::{eq_band_can_shape_gain, ParamChange};
use super::{MasterAssistant, MasterAssistantReport, MasteringSuite};
use crate::audio::buffer::AudioBuffer;
use crate::audio::separator::Radix2Fft;

/// Longest request passed to the model
pub const MAX_REQUEST_CHARS: usize = 200;
pub const MAX_EDITS: usize = 4;
/// Enough for `MAX_EDITS` edits and a one-line note
pub const MAX_TOKENS: usize = 160;

const EQ_PARAMS_PER_BAND: usize = 9;
const EQ_GAIN: usize = 3;
const SHAPE_FFT: usize = 4096;
const SHAPE_FRAMES: usize = 200;

/// A parameter a request may move
struct TextParam {
    key: &'static str,
    description: &'static str,
    kind: ModuleKind,
    index: usize,
    /// Parameter units per unit of the model's `change` (percentages are stored as 0..1)
    scale: f32,
    /// Largest `change` in one request
    limit: f32,
}

const fn eq_gain(key: &'static str, description: &'static str, band: usize) -> TextParam {
    TextParam { key, description, kind: ModuleKind::Equalizer, index: band * EQ_PARAMS_PER_BAND + EQ_GAIN, scale: 1.0, limit: 3.0 }
}

const TEXT_PARAMS: [TextParam; 9] = [
    eq_gain("bass_db", "low shelf near 75 Hz: more or less weight and punch", 1),
    eq_gain("low_mid_db", "bell near 180 Hz: positive adds warmth, negative removes mud", 2),
    eq_gain("mid_db", "bell near 650 Hz: negative removes boxiness", 3),
    eq_gain("upper_mid_db", "bell near 1.8 kHz: positive adds bite, negative softens", 4),
    eq_gain("presence_db", "bell near 4.5 kHz: positive adds clarity, negative removes harshness", 5),
    eq_gain("air_db", "high shelf near 11.5 kHz: positive adds air and brightness, negative darkens", 6),
    TextParam { key: "width_percent", description: "stereo width: positive is wider, negative narrower", kind: ModuleKind::Imager, index: 0, scale: 0.01, limit: 25.0 },
    TextParam { key: "saturation_percent", description: "harmonic exciter drive: positive is more saturated and edgy", kind: ModuleKind::Exciter, index: 1, scale: 0.01, limit: 15.0 },
    TextParam { key: "compression_threshold_db", description: "compressor threshold: negative compresses harder, positive is more open", kind: ModuleKind::VintageCompressor, index: 0, scale: 1.0, limit: 6.0 },
];

/// Worked examples, sent as earlier turns of the conversation: request and answer
const EXAMPLES: [(&str, &str); 5] = [
    ("warmer, less harsh", r#"{"edits":[{"param":"low_mid_db","change":1.5},{"param":"presence_db","change":-2}],"note":"More warmth, softer upper presence."}"#),
    ("wider and brighter", r#"{"edits":[{"param":"width_percent","change":15},{"param":"air_db","change":1.5}],"note":"Wider image with more air."}"#),
    ("darker, less bright", r#"{"edits":[{"param":"air_db","change":-2},{"param":"presence_db","change":-1}],"note":"Rolled off the top end."}"#),
    ("tighter and punchier", r#"{"edits":[{"param":"low_mid_db","change":-1.5},{"param":"bass_db","change":1}],"note":"Less mud with a little more weight."}"#),
    ("a bit narrower, less muddy", r#"{"edits":[{"param":"width_percent","change":-10},{"param":"low_mid_db","change":-2}],"note":"Narrower image, cleaner low-mids."}"#),
];

#[derive(Deserialize)]
struct Answer {
    edits: Vec<AnswerEdit>,
    #[serde(default)]
    note: String,
}

#[derive(Deserialize)]
struct AnswerEdit {
    param: String,
    change: f32,
}

/// Brightness and noisiness of a track: spectral centroid in Hz, and flatness (0 tonal, 1 noise)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectralShape {
    pub centroid_hz: f32,
    pub flatness: f32,
}

/// Measure the shape from frames spread evenly over the track
pub fn spectral_shape(track: &AudioBuffer) -> SpectralShape {
    let (left, right) = track.to_stereo();
    let fft = Radix2Fft::new(SHAPE_FFT);
    let window: Vec<f32> = (0..SHAPE_FFT)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / SHAPE_FFT as f32).cos()))
        .collect();
    let bins = SHAPE_FFT / 2;
    let mut power = vec![0.0f64; bins];
    let span = left.len().saturating_sub(SHAPE_FFT);
    let (mut re, mut im) = (vec![0.0f32; SHAPE_FFT], vec![0.0f32; SHAPE_FFT]);
    if left.len() >= SHAPE_FFT {
        for frame in 0..SHAPE_FRAMES {
            let start = span * frame / (SHAPE_FRAMES - 1);
            for i in 0..SHAPE_FFT {
                re[i] = (left[start + i] + right[start + i]) * 0.5 * window[i];
            }
            im.fill(0.0);
            fft.forward(&mut re, &mut im);
            for (k, p) in power.iter_mut().enumerate() {
                *p += (re[k] * re[k] + im[k] * im[k]) as f64;
            }
        }
    }
    let hz_per_bin = track.sample_rate as f64 / SHAPE_FFT as f64;
    let total: f64 = power[1..].iter().sum();
    if total <= 1e-12 {
        return SpectralShape { centroid_hz: 0.0, flatness: 0.0 };
    }
    let centroid: f64 = power.iter().enumerate().skip(1).map(|(k, p)| k as f64 * hz_per_bin * p).sum::<f64>() / total;
    let log_mean = power[1..].iter().map(|p| p.max(1e-20).ln()).sum::<f64>() / (bins - 1) as f64;
    let flatness = log_mean.exp() / (total / (bins - 1) as f64);
    SpectralShape { centroid_hz: centroid as f32, flatness: flatness as f32 }
}

/// A suite parameter move
#[derive(Clone, Debug, PartialEq)]
pub struct TextEdit {
    pub kind: ModuleKind,
    pub index: usize,
    pub value: f32,
}

/// What a request came to
#[derive(Clone, Debug)]
pub struct TextPlan {
    pub edits: Vec<TextEdit>,
    pub changes: Vec<ParamChange>,
    pub note: String,
}

impl TextPlan {
    pub fn apply(&self, suite: &mut MasteringSuite) {
        for e in &self.edits {
            suite.module_mut(e.kind).set_param(e.index, e.value);
        }
    }

    pub fn to_text(&self) -> String {
        let mut text = String::new();
        if !self.note.trim().is_empty() {
            let _ = writeln!(text, "{}\n", self.note.trim());
        }
        for c in &self.changes {
            let _ = writeln!(text, "{}: {} to {}", c.name, c.before, c.after);
        }
        text
    }
}

fn available(suite: &MasteringSuite) -> Vec<&'static TextParam> {
    TEXT_PARAMS
        .iter()
        .filter(|p| suite.module(p.kind).enabled())
        .filter(|p| p.kind != ModuleKind::Equalizer || eq_band_can_shape_gain(suite, p.index / EQ_PARAMS_PER_BAND))
        .collect()
}

/// GBNF for an answer: up to `MAX_EDITS` edits of the parameters available in `suite`, and a note
pub fn grammar(suite: &MasteringSuite) -> String {
    let keys: Vec<String> = available(suite).iter().map(|p| format!("\"\\\"{}\\\"\"", p.key)).collect();
    format!(
        r#"root ::= "{{\"edits\":[" edit ("," edit){{0,{more}}} "],\"note\":\"" note "\"}}"
edit ::= "{{\"param\":" key ",\"change\":" number "}}"
key ::= {keys}
number ::= "-"? [0-9] [0-9]? ("." [0-9])?
note ::= [a-zA-Z0-9 ,.'%+-]{{0,100}}
"#,
        more = MAX_EDITS - 1,
        keys = keys.join(" | ")
    )
}

/// The system prompt: the rules and the parameters as they stand
pub fn system_prompt(suite: &MasteringSuite) -> String {
    let mut text = String::from(
        "You are a mastering engineer. Turn the user's request into a few small moves of the parameters below. \
         Move one parameter for each thing the request asks for, in the direction it asks for, and nothing else. \
         Answer with JSON only.\n\nParameters:\n",
    );
    for p in available(suite) {
        let module = suite.module(p.kind);
        let info = &module.params()[p.index];
        let _ = writeln!(text, "- {}: {} (now {}, change at most +-{})", p.key, p.description, info.format(module.get_param(p.index)), p.limit);
    }
    text
}

/// The conversation for a request: system prompt, worked examples, then the request
pub fn chat(suite: &MasteringSuite, report: &MasterAssistantReport, shape: SpectralShape, request: &str) -> Vec<(&'static str, String)> {
    let mut chat = vec![("system", system_prompt(suite))];
    for (example, answer) in EXAMPLES {
        chat.push(("user", format!("Request: {}", example)));
        chat.push(("assistant", answer.to_string()));
    }
    chat.push(("user", user_prompt(report, shape, request)));
    chat
}

/// The user message: the track's measurements and the request
pub fn user_prompt(report: &MasterAssistantReport, shape: SpectralShape, request: &str) -> String {
    let request: String = request.chars().take(MAX_REQUEST_CHARS).collect();
    format!(
        "Track: {:.1} LUFS, crest factor {:.1} dB, low-end phase correlation {:.2}, spectral centroid {:.0} Hz, spectral flatness {:.3}.\nRequest: {}",
        report.current_integrated_lufs, report.crest_factor_db, report.sub_bass_phase_corr, shape.centroid_hz, shape.flatness, request.trim()
    )
}

/// Read the model's answer. Unknown parameters are dropped, and each move is limited, snapped to
/// the slider grid and kept inside the slider's range.
pub fn parse_answer(answer: &str, suite: &MasteringSuite) -> Result<TextPlan, String> {
    let parsed: Answer = serde_json::from_str(answer).map_err(|_| "The text model gave an unreadable answer".to_string())?;
    let params = available(suite);
    let mut plan = TextPlan { edits: Vec::new(), changes: Vec::new(), note: parsed.note };
    for edit in parsed.edits.iter().take(MAX_EDITS) {
        let Some(p) = params.iter().find(|p| p.key == edit.param) else { continue };
        // Snapping alone would nudge a setting that's off the grid
        if !edit.change.is_finite() || edit.change.abs() < 1e-3 || plan.edits.iter().any(|e| e.kind == p.kind && e.index == p.index) {
            continue;
        }
        let module = suite.module(p.kind);
        let info: &ParamInfo = &module.params()[p.index];
        let before = module.get_param(p.index);
        let value = info.snapped(before + edit.change.clamp(-p.limit, p.limit) * p.scale);
        if (value - before).abs() > 1e-4 {
            plan.edits.push(TextEdit { kind: p.kind, index: p.index, value });
            plan.changes.push(ParamChange { name: format!("{} › {}", p.kind.name(), info.name), before: info.format(before), after: info.format(value) });
        }
    }
    Ok(plan)
}

/// Turn `request` into a plan for `suite`. `generate` is the model: it takes the conversation and
/// the grammar. Blocking: run it on a worker thread.
pub fn plan_request(
    track: &AudioBuffer,
    suite: &MasteringSuite,
    request: &str,
    target_lufs: f32,
    generate: impl FnOnce(&[(&str, String)], &str) -> Result<String, String>,
) -> Result<TextPlan, String> {
    if request.trim().is_empty() {
        return Err("Describe what you'd like to change".to_string());
    }
    if available(suite).is_empty() {
        return Err("No module to adjust: switch on the Equalizer, Stereo Imager, Harmonic Exciter or Vintage Compressor".to_string());
    }
    let report = MasterAssistant::analyze(track, target_lufs)?;
    let shape = spectral_shape(track);
    let answer = generate(&chat(suite, &report, shape, request), &grammar(suite))?;
    parse_answer(&answer, suite)
}
