//! `Module` implementations for the original suite processors (EQ, dynamics, imager, exciter,
//! clarity, maximizer), exposing their parameters to the editor and presets.

use super::module::{Module, ParamInfo, ParamUnit};
use super::{
    DynamicEqualizer, EqChannelMode, EqFilterType, MasteringExciter, MasteringImager, MasteringMaximizer,
    MultibandDynamics, SpectralClarity, EXCITER_MODES,
};

const EQ_SHAPES: &[&str] = &["High-pass", "Low shelf", "Bell", "High shelf", "Low-pass"];
const EQ_SHAPE_TYPES: [EqFilterType; 5] = [
    EqFilterType::HighPass,
    EqFilterType::LowShelf,
    EqFilterType::Peaking,
    EqFilterType::HighShelf,
    EqFilterType::LowPass,
];
const EQ_CHANNELS: &[&str] = &["Stereo", "Mid", "Side"];
const EQ_CHANNEL_MODES: [EqChannelMode; 3] = [EqChannelMode::Stereo, EqChannelMode::MidOnly, EqChannelMode::SideOnly];
const EQ_PARAMS_PER_BAND: usize = 9;

fn flag(v: bool) -> f32 {
    if v { 1.0 } else { 0.0 }
}

fn index(v: f32, len: usize) -> usize {
    (v.round().max(0.0) as usize).min(len - 1)
}

impl Module for DynamicEqualizer {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        let mut p = Vec::new();
        for b in 1..=self.bands.len() {
            p.push(ParamInfo::toggle(format!("B{} on", b)));
            p.push(ParamInfo::choice(format!("B{} shape", b), EQ_SHAPES));
            p.push(ParamInfo::hz(format!("B{} frequency", b), 20.0, 22000.0));
            p.push(ParamInfo::db(format!("B{} gain", b), -18.0, 18.0));
            p.push(ParamInfo { log: true, ..ParamInfo::new(format!("B{} Q", b), ParamUnit::Plain, 0.2, 10.0, 0.06) });
            p.push(ParamInfo::choice(format!("B{} channel", b), EQ_CHANNELS));
            p.push(ParamInfo::toggle(format!("B{} dynamic", b)));
            p.push(ParamInfo::db(format!("B{} dyn threshold", b), -60.0, 0.0));
            p.push(ParamInfo::db(format!("B{} dyn range", b), -12.0, 12.0));
        }
        p
    }
    fn get_param(&self, i: usize) -> f32 {
        let b = &self.bands[(i / EQ_PARAMS_PER_BAND).min(self.bands.len() - 1)];
        match i % EQ_PARAMS_PER_BAND {
            0 => flag(b.enabled),
            1 => EQ_SHAPE_TYPES.iter().position(|t| *t == b.filter_type).unwrap_or(2) as f32,
            2 => b.freq,
            3 => b.static_gain_db,
            4 => b.q,
            5 => EQ_CHANNEL_MODES.iter().position(|m| *m == b.channel_mode).unwrap_or(0) as f32,
            6 => flag(b.dynamic_enabled),
            7 => b.threshold_db,
            _ => b.dynamic_range_db,
        }
    }
    fn set_param(&mut self, i: usize, v: f32) {
        let n = self.bands.len();
        let b = &mut self.bands[(i / EQ_PARAMS_PER_BAND).min(n - 1)];
        match i % EQ_PARAMS_PER_BAND {
            0 => b.enabled = v > 0.5,
            1 => b.filter_type = EQ_SHAPE_TYPES[index(v, 5)],
            2 => b.freq = v.clamp(20.0, 22000.0),
            3 => b.static_gain_db = v.clamp(-18.0, 18.0),
            4 => b.q = v.clamp(0.2, 10.0),
            5 => {
                b.channel_mode = EQ_CHANNEL_MODES[index(v, 3)];
                b.reset_state();
            }
            6 => b.dynamic_enabled = v > 0.5,
            7 => b.threshold_db = v.clamp(-60.0, 0.0),
            _ => b.dynamic_range_db = v.clamp(-12.0, 12.0),
        }
        b.update_coefficients(b.static_gain_db);
    }
    fn set_sample_rate(&mut self, sr: f32) { DynamicEqualizer::set_sample_rate(self, sr); }
    fn reset(&mut self) { self.reset_state(); }
    fn process(&mut self, l: &mut [f32], r: &mut [f32]) { self.process_block(l, r); }
}

const DYN_BANDS: [&str; 4] = ["Low", "Low-mid", "High-mid", "High"];

impl Module for MultibandDynamics {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        let mut p = Vec::new();
        for name in DYN_BANDS {
            p.push(ParamInfo::toggle(format!("{} on", name)));
            p.push(ParamInfo::db(format!("{} threshold", name), -60.0, 0.0));
            p.push(ParamInfo::new(format!("{} ratio", name), ParamUnit::Ratio, 1.0, 20.0, 0.1));
            p.push(ParamInfo::new(format!("{} attack", name), ParamUnit::Ms, 0.1, 500.0, 1.0));
            p.push(ParamInfo::new(format!("{} release", name), ParamUnit::Ms, 1.0, 2000.0, 10.0));
            p.push(ParamInfo::db(format!("{} makeup", name), -12.0, 12.0));
        }
        p
    }
    fn get_param(&self, i: usize) -> f32 {
        let b = &self.bands[(i / 6).min(3)];
        [flag(b.enabled), b.threshold_db, b.ratio, b.attack_ms, b.release_ms, b.makeup_gain_db][i % 6]
    }
    fn set_param(&mut self, i: usize, v: f32) {
        let b = &mut self.bands[(i / 6).min(3)];
        match i % 6 {
            0 => b.enabled = v > 0.5,
            1 => b.threshold_db = v.clamp(-60.0, 0.0),
            2 => b.ratio = v.clamp(1.0, 20.0),
            3 => b.attack_ms = v.clamp(0.1, 500.0),
            4 => b.release_ms = v.clamp(1.0, 2000.0),
            _ => b.makeup_gain_db = v.clamp(-12.0, 12.0),
        }
    }
    fn set_sample_rate(&mut self, sr: f32) { MultibandDynamics::set_sample_rate(self, sr); }
    fn reset(&mut self) { MultibandDynamics::reset(self); }
    fn process(&mut self, l: &mut [f32], r: &mut [f32]) { self.process_block(l, r); }
    fn status(&self) -> Option<String> {
        let gr: Vec<String> = self.bands.iter().map(|b| format!("{:.1}", b.gain_reduction_db.abs())).collect();
        Some(format!("Gain reduction (dB): {}", gr.join(" / ")))
    }
}

impl Module for MasteringImager {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        vec![
            ParamInfo::percent("Overall width", 0.0, 2.0),
            ParamInfo::hz("Mono below", 20.0, 500.0),
            ParamInfo::percent("Width < 200 Hz", 0.0, 2.0),
            ParamInfo::percent("Width 200 Hz-2 kHz", 0.0, 2.0),
            ParamInfo::percent("Width 2-8 kHz", 0.0, 2.0),
            ParamInfo::percent("Width > 8 kHz", 0.0, 2.0),
            ParamInfo::percent("Stereoize", 0.0, 1.0),
        ]
    }
    fn get_param(&self, i: usize) -> f32 {
        match i {
            0 => self.width,
            1 => self.mono_bass_freq,
            2..=5 => self.band_width[i - 2],
            _ => self.stereoize,
        }
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.width = v.clamp(0.0, 2.0),
            1 => self.set_mono_cutoff(v.clamp(20.0, 500.0)),
            2..=5 => self.band_width[i - 2] = v.clamp(0.0, 2.0),
            _ => self.stereoize = v.clamp(0.0, 1.0),
        }
    }
    fn set_sample_rate(&mut self, sr: f32) { MasteringImager::set_sample_rate(self, sr); }
    fn reset(&mut self) { self.reset_state(); }
    fn process(&mut self, l: &mut [f32], r: &mut [f32]) { self.process_block(l, r); }
}

impl Module for MasteringExciter {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        vec![
            ParamInfo::choice("Mode", EXCITER_MODES),
            ParamInfo::percent("Drive", 0.0, 1.0),
            ParamInfo::percent("Mix", 0.0, 1.0),
            ParamInfo::percent("Warmth (even harmonics)", 0.0, 1.0),
            ParamInfo::hz("Excite above", 500.0, 12000.0),
        ]
    }
    fn get_param(&self, i: usize) -> f32 {
        [self.mode as f32, self.drive, self.mix, self.warmth, self.crossover_hz][i.min(4)]
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.mode = index(v, EXCITER_MODES.len()),
            1 => self.drive = v.clamp(0.0, 1.0),
            2 => self.mix = v.clamp(0.0, 1.0),
            3 => self.warmth = v.clamp(0.0, 1.0),
            _ => self.set_crossover(v),
        }
    }
    fn set_sample_rate(&mut self, sr: f32) { MasteringExciter::set_sample_rate(self, sr); }
    fn reset(&mut self) { self.reset_state(); }
    fn process(&mut self, l: &mut [f32], r: &mut [f32]) { self.process_block(l, r); }
}

impl Module for SpectralClarity {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        vec![ParamInfo::percent("Amount", 0.0, 1.0), ParamInfo::hz("Focus", 500.0, 16000.0)]
    }
    fn get_param(&self, i: usize) -> f32 {
        if i == 0 { self.amount } else { self.focus_freq }
    }
    fn set_param(&mut self, i: usize, v: f32) {
        if i == 0 { self.amount = v.clamp(0.0, 1.0) } else { self.set_focus_freq(v) }
    }
    fn set_sample_rate(&mut self, sr: f32) { SpectralClarity::set_sample_rate(self, sr); }
    fn reset(&mut self) { SpectralClarity::reset(self); }
    fn process(&mut self, l: &mut [f32], r: &mut [f32]) { self.process_block(l, r); }
}

impl Module for MasteringMaximizer {
    fn enabled(&self) -> bool { self.enabled }
    fn set_enabled(&mut self, e: bool) { self.enabled = e; }
    fn params(&self) -> Vec<ParamInfo> {
        vec![
            ParamInfo::db("Drive", 0.0, 30.0),
            ParamInfo::new("Ceiling", ParamUnit::Db, -12.0, 0.0, 0.1),
            ParamInfo::new("Release", ParamUnit::Ms, 10.0, 300.0, 5.0),
            ParamInfo::toggle("Adaptive release"),
        ]
    }
    fn get_param(&self, i: usize) -> f32 {
        match i {
            0 => -self.threshold_db,
            1 => self.ceiling_db,
            2 => self.release_ms,
            _ => flag(self.irc_adaptive),
        }
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.threshold_db = -v.clamp(0.0, 30.0),
            1 => self.ceiling_db = v.clamp(-12.0, 0.0),
            2 => self.release_ms = v.clamp(10.0, 300.0),
            _ => self.irc_adaptive = v > 0.5,
        }
    }
    fn set_sample_rate(&mut self, sr: f32) { MasteringMaximizer::set_sample_rate(self, sr); }
    fn reset(&mut self) { MasteringMaximizer::reset(self); }
    fn process(&mut self, l: &mut [f32], r: &mut [f32]) { self.process_block(l, r); }
    fn latency(&self) -> usize { self.latency_samples() }
    fn status(&self) -> Option<String> {
        Some(format!("Gain reduction: {:.1} dB", self.gain_reduction_db.abs()))
    }
}
