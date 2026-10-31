//! CPU cost of each module (and of whole chains) at 48 kHz stereo, 512-frame blocks.
//!
//! `cargo run --release --example bench_modules`

use std::time::Instant;

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
}
