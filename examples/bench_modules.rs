//! CPU cost of each module (and of whole chains) at 48 kHz stereo, 512-frame blocks.
//!
//! `cargo run --release --example bench_modules`

use std::time::Instant;

use mastering_studio::audio::{AudioBuffer, StyleFrontEnd, StyleModel};
use mastering_studio::dsp::style_match::{prepare_excerpts, render_candidate};
use mastering_studio::dsp::{MasteringSuite, ModuleKind};

const SR: f32 = 48000.0;
const SECONDS: f32 = 10.0;
const BLOCK: usize = 512;

fn signal() -> (Vec<f32>, Vec<f32>) {
    let n = (SR * SECONDS) as usize;
    let mut seed: u64 = 1;
    let mut l = Vec::with_capacity(n);
    let mut r = Vec::with_capacity(n);
    for i in 0..n {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let noise = ((seed >> 33) as f32 / (1u64 << 31) as f32 * 2.0 - 1.0) * 0.1;
        let tone = (i as f32 * 2.0 * std::f32::consts::PI * 110.0 / SR).sin() * 0.3;
        l.push(tone + noise);
        r.push(tone - noise);
    }
    (l, r)
}

/// Seconds of CPU per second of audio
fn measure(suite: &mut MasteringSuite, input: &(Vec<f32>, Vec<f32>)) -> f64 {
    let (mut l, mut r) = input.clone();
    // Warm up (spectral modules fill their buffers, allocations settle)
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)).take(200) {
        suite.process_stereo(cl, cr);
    }
    let (mut l, mut r) = input.clone();
    let started = Instant::now();
    for (cl, cr) in l.chunks_mut(BLOCK).zip(r.chunks_mut(BLOCK)) {
        suite.process_stereo(cl, cr);
    }
    started.elapsed().as_secs_f64() / SECONDS as f64
}

fn bare() -> MasteringSuite {
    let mut s = MasteringSuite::new(SR);
    for &k in ModuleKind::ALL {
        s.module_mut(k).set_enabled(false);
    }
    s
}

fn row(name: &str, load: f64) {
    if load < 1e-5 {
        println!("{:<34} {:>18}   (passes audio through at default settings)", name, "~0%");
        return;
    }
    println!("{:<34} {:>7.2}% of a core   {:>8.0}x real time", name, load * 100.0, 1.0 / load.max(1e-9));
}

/// Time one Match reference (AI) candidate: its excerpts through the default chain and, when the
/// model is present, the embeddings
fn style_eval(input: &(Vec<f32>, Vec<f32>)) {
    let track = AudioBuffer::from_stereo(input.0.clone(), input.1.clone(), SR as u32);
    let mut suite = MasteringSuite::new(SR);
    let latency = suite.latency_samples();
    let excerpts = prepare_excerpts(&track, None, &suite, latency);
    let front_end = StyleFrontEnd::default();
    let started = Instant::now();
    let features = render_candidate(&mut suite, &excerpts, &front_end).expect("render failed");
    let render = started.elapsed().as_secs_f64();
    let model = mastering_studio::audio::ml_style::find_model().map(|p| StyleModel::load(&p).expect("model failed to load"));
    match model {
        Some(mut model) => {
            let started = Instant::now();
            model.embed_features(&features).expect("embed failed");
            let embed = started.elapsed().as_secs_f64();
            println!("{:<34} {:>7.0} ms render + {:.0} ms embed ({} excerpts)", "style_eval", render * 1e3, embed * 1e3, excerpts.len());
        }
        None => println!("{:<34} {:>7.0} ms render, no model to embed ({} excerpts)", "style_eval", render * 1e3, excerpts.len()),
    }
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("Note: this is a debug build; use --release for meaningful numbers");
    }
    let input = signal();
    println!("{:<34} {:>18}   {:>18}", "module (48 kHz stereo)", "CPU", "speed");
    let baseline = measure(&mut bare(), &input);
    for &kind in ModuleKind::ALL {
        let mut s = bare();
        s.module_mut(kind).set_enabled(true);
        let os = s.oversampling[kind.index()];
        let label = if os > 1 { format!("{} ({}x OS)", kind.name(), os) } else { kind.name().to_string() };
        row(&label, (measure(&mut s, &input) - baseline).max(0.0));
    }
    println!();
    row("Default chain", measure(&mut MasteringSuite::new(SR), &input));
    let mut all = MasteringSuite::new(SR);
    for &k in ModuleKind::ALL {
        all.module_mut(k).set_enabled(true);
    }
    row("Every module on", measure(&mut all, &input));
    println!();
    style_eval(&input);
}
