//! Mastering suite: the module chain and the selected module's editor

use eframe::egui::{self, Color32, RichText, Vec2};

use super::app::{MasteringApp, MidiTarget};
use super::widgets::{self, ACCENT, DIM, GOOD, WARN};
use crate::audio::{MeterLevels, StemStatus};
use crate::dsp::{Module, ModuleKind, ParamInfo, ParamUnit};

/// Everything the view needs from the suite, read under one lock
struct ViewModel {
    chain: Vec<(ModuleKind, bool, bool)>,
    params: Vec<(ParamInfo, f32)>,
    enabled: bool,
    auto_gain: bool,
    auto_gain_db: f32,
    /// Oversampling factor, when the module offers it
    oversampling: Option<usize>,
    status: Option<String>,
    meters: [f32; 4],
    latency: usize,
    sample_rate: f32,
    eq: Option<crate::dsp::DynamicEqualizer>,
}

pub fn show(app: &mut MasteringApp, ui: &mut egui::Ui, meters: &MeterLevels) {
    let selected = app.selected_module;
    let vm = app.engine.with_dsp_suite(|s| {
        let m = s.module(selected);
        ViewModel {
            chain: s.chain.iter().map(|&k| (k, s.module(k).enabled(), s.auto_gain[k.index()])).collect(),
            params: m.params().into_iter().enumerate().map(|(i, p)| (p, m.get_param(i))).collect(),
            enabled: m.enabled(),
            auto_gain: s.auto_gain[selected.index()],
            auto_gain_db: s.auto_gain_db(selected),
            oversampling: s.supports_oversampling(selected).then(|| s.oversampling[selected.index()]),
            status: m.status(),
            meters: m.meters(),
            latency: s.latency_samples(),
            sample_rate: s.sample_rate,
            eq: (selected == ModuleKind::Equalizer).then(|| s.dynamic_eq.clone()),
        }
    });

    egui::SidePanel::left("chain").resizable(false).exact_width(270.0).show_inside(ui, |ui| {
        chain_list(app, ui, &vm);
    });
    egui::CentralPanel::default().show_inside(ui, |ui| {
        module_editor(app, ui, &vm, meters);
    });
}

fn chain_list(app: &mut MasteringApp, ui: &mut egui::Ui, vm: &ViewModel) {
    ui.label(RichText::new("SIGNAL CHAIN").color(DIM).size(11.0));
    ui.label(RichText::new("Tick to switch a module on • arrows reorder").color(DIM).size(10.0));
    ui.add_space(4.0);
    let n = vm.chain.len();
    egui::ScrollArea::vertical().show(ui, |ui| {
        for (pos, &(kind, on, auto)) in vm.chain.iter().enumerate() {
            ui.horizontal(|ui| {
                let mut enabled = on;
                if ui.checkbox(&mut enabled, "").on_hover_text("Switch this module on or off").changed() {
                    app.edit_suite(&format!("{}/enabled", kind.key()), move |s| s.module_mut(kind).set_enabled(enabled));
                }
                let mut text = RichText::new(format!("{:>2}. {}", pos + 1, kind.name()));
                if !on {
                    text = text.color(DIM);
                }
                let resp = ui.selectable_label(app.selected_module == kind, text);
                if resp.clicked() {
                    app.selected_module = kind;
                }
                if auto {
                    ui.label(RichText::new("AG").color(GOOD).size(10.0)).on_hover_text("Auto gain-match is on");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(pos + 1 < n, egui::Button::new("⏷").small()).clicked() {
                        app.edit_suite("chain", move |s| s.move_module(kind, 1));
                    }
                    if ui.add_enabled(pos > 0, egui::Button::new("⏶").small()).clicked() {
                        app.edit_suite("chain", move |s| s.move_module(kind, -1));
                    }
                });
            });
        }
        ui.add_space(8.0);
        let ms = vm.latency as f32 / vm.sample_rate.max(1.0) * 1000.0;
        ui.label(RichText::new(format!("Chain latency: {} samples ({:.1} ms)", vm.latency, ms)).color(DIM).size(11.0));
    });
}

fn module_editor(app: &mut MasteringApp, ui: &mut egui::Ui, vm: &ViewModel, meters: &MeterLevels) {
    let kind = app.selected_module;
    ui.horizontal(|ui| {
        ui.heading(RichText::new(kind.name()).color(if vm.enabled { ACCENT } else { DIM }));
        let mut enabled = vm.enabled;
        let label = if enabled { "ON" } else { "OFF" };
        if ui.toggle_value(&mut enabled, label).changed() {
            app.edit_suite(&format!("{}/enabled", kind.key()), move |s| s.module_mut(kind).set_enabled(enabled));
        }
        let mut auto = vm.auto_gain;
        let label = if auto { format!("Auto gain-match ({:+.1} dB)", vm.auto_gain_db) } else { "Auto gain-match".to_string() };
        if ui.toggle_value(&mut auto, label).on_hover_text("Trim this module's output so it changes tone, not level").changed() {
            app.edit_suite(&format!("{}/auto", kind.key()), move |s| s.set_auto_gain(kind, auto));
        }
        if let Some(current) = vm.oversampling {
            let label = |f: usize| if f == 1 { "Oversampling: off".to_string() } else { format!("Oversampling: {}x", f) };
            let mut chosen = current;
            egui::ComboBox::from_id_salt("oversampling").selected_text(label(current)).show_ui(ui, |ui| {
                for f in [1usize, 2, 4] {
                    ui.selectable_value(&mut chosen, f, label(f));
                }
            })
            .response
            .on_hover_text("Run this module at a higher rate so its distortion doesn't alias (adds a little latency)");
            if chosen != current {
                app.edit_suite(&format!("{}/oversampling", kind.key()), move |s| s.set_oversampling(kind, chosen));
            }
        }
        if kind == ModuleKind::MatchEq && ui.button("Learn from reference track").clicked() {
            app.learn_match_eq();
        }
        if matches!(kind, ModuleKind::StemRebalance | ModuleKind::StemEq) {
            match app.engine.stem_status() {
                StemStatus::Unavailable => {
                    ui.label(RichText::new("Real-time approximation (no model in models/)").color(DIM));
                }
                StemStatus::NotSeparated => {
                    if ui.button("Separate stems (AI)").clicked() {
                        app.separate_stems_ai();
                    }
                    ui.label(RichText::new("Real-time approximation until separated").color(WARN));
                }
                StemStatus::Separating(p) => {
                    ui.spinner();
                    ui.label(RichText::new(format!("Separating: {:.0}%", p * 100.0)).color(WARN));
                }
                StemStatus::Ready => {
                    ui.label(RichText::new("Using AI-separated stems").color(GOOD));
                }
            }
        }
    });
    if let Some(ref s) = vm.status {
        ui.label(RichText::new(s).color(GOOD));
    }
    ui.add_space(4.0);

    if let Some(ref eq) = vm.eq {
        let top = (vm.sample_rate * 0.48).min(22000.0);
        let size = Vec2::new(ui.available_width(), 230.0);
        if let Some(edit) = widgets::eq_curve(ui, eq, &meters.spectrum_db, top, &mut app.selected_band, size) {
            let band = edit.band;
            let (f, g, q) = (edit.freq, edit.gain_db, edit.q);
            app.edit_suite(&format!("equalizer/node{}", band), move |s| {
                let base = band * 9;
                s.dynamic_eq.set_param(base + 2, f);
                s.dynamic_eq.set_param(base + 3, g);
                s.dynamic_eq.set_param(base + 4, q);
            });
        }
        ui.label(RichText::new("Drag a node to move it • scroll over a node for Q • the shaded area is the live spectrum").color(DIM).size(11.0));
        ui.add_space(6.0);
    }

    meter_section(ui, kind, &vm.meters);

    // Parameters: the selected EQ band first, then everything (grouped in columns)
    let mut changes: Vec<(usize, f32)> = Vec::new();
    let mut learn: Option<usize> = None;
    let mut unmap: Option<usize> = None;
    let mapped: Vec<usize> = app
        .midi_map
        .values()
        .filter_map(|t| match t {
            MidiTarget::Param(k, i) if *k == kind => Some(*i),
            _ => None,
        })
        .collect();
    let indices: Vec<usize> = match (kind, app.selected_band) {
        (ModuleKind::Equalizer, Some(b)) => (b * 9..b * 9 + 9).collect(),
        _ => (0..vm.params.len()).collect(),
    };
    if kind == ModuleKind::Equalizer {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Band").color(DIM));
            for b in 0..8 {
                if ui.selectable_label(app.selected_band == Some(b), format!("{}", b + 1)).clicked() {
                    app.selected_band = Some(b);
                }
            }
            if ui.selectable_label(app.selected_band.is_none(), "All").clicked() {
                app.selected_band = None;
            }
        });
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        let cols = if indices.len() > 12 { 3 } else { 1 };
        let per_col = (indices.len() + cols - 1) / cols.max(1);
        ui.columns(cols, |columns| {
            for (c, col) in columns.iter_mut().enumerate() {
                egui::Grid::new(format!("params{}", c)).num_columns(2).spacing([10.0, 6.0]).show(col, |ui| {
                    for &i in indices.iter().skip(c * per_col).take(per_col) {
                        let Some((info, value)) = vm.params.get(i) else { continue };
                        let mut label = RichText::new(&info.name);
                        if !vm.enabled {
                            label = label.color(DIM);
                        }
                        if mapped.contains(&i) {
                            label = label.color(WARN);
                        }
                        let resp = ui.label(label);
                        resp.context_menu(|ui| {
                            if ui.button("MIDI learn").clicked() {
                                learn = Some(i);
                                ui.close_menu();
                            }
                            if mapped.contains(&i) && ui.button("Remove MIDI mapping").clicked() {
                                unmap = Some(i);
                                ui.close_menu();
                            }
                        });
                        if let Some(v) = param_widget(ui, info, *value) {
                            changes.push((i, v));
                        }
                        ui.end_row();
                    }
                });
            }
        });
    });
    for (i, v) in changes {
        app.set_param(kind, i, v);
    }
    if let Some(i) = learn {
        app.midi_learn = Some(MidiTarget::Param(kind, i));
        app.status.set(format!("MIDI learn armed for {} › {} – move a controller", kind.name(), vm.params[i].0.name));
    }
    if let Some(i) = unmap {
        app.midi_map.retain(|_, t| *t != MidiTarget::Param(kind, i));
    }
}

/// The widget for one parameter; returns a new value when the user changed it
fn param_widget(ui: &mut egui::Ui, info: &ParamInfo, value: f32) -> Option<f32> {
    match info.unit {
        ParamUnit::Toggle => {
            let mut on = value > 0.5;
            ui.checkbox(&mut on, "").changed().then_some(if on { 1.0 } else { 0.0 })
        }
        ParamUnit::Choice(opts) => {
            let mut idx = value.round().max(0.0) as usize;
            let before = idx;
            egui::ComboBox::from_id_salt(ui.next_auto_id()).selected_text(opts.get(idx).copied().unwrap_or("?")).width(150.0).show_ui(ui, |ui| {
                for (k, o) in opts.iter().enumerate() {
                    ui.selectable_value(&mut idx, k, *o);
                }
            });
            (idx != before).then_some(idx as f32)
        }
        _ => {
            let mut v = value;
            let fmt = info.clone();
            let slider = egui::Slider::new(&mut v, info.min..=info.max)
                .logarithmic(info.log && info.min > 0.0)
                .custom_formatter(move |x, _| fmt.format(x as f32))
                .clamping(egui::SliderClamping::Always);
            let resp = ui.add_sized([230.0, 18.0], slider);
            resp.changed().then_some(v)
        }
    }
}

fn meter_section(ui: &mut egui::Ui, kind: ModuleKind, m: &[f32; 4]) {
    match kind {
        ModuleKind::Maximizer | ModuleKind::VintageCompressor | ModuleKind::OptoLimiter => {
            widgets::gr_bar(ui, "Gain reduction", m[0], 12.0);
        }
        ModuleKind::Dynamics => {
            for (i, name) in ["Low", "Low-mid", "High-mid", "High"].iter().enumerate() {
                widgets::gr_bar(ui, name, m[i], 12.0);
            }
        }
        ModuleKind::SpectralShaper => widgets::gr_bar(ui, "Peak reduction", m[0], 12.0),
        ModuleKind::TransientRestore => widgets::gr_bar(ui, "Transient boost", m[0], 12.0),
        ModuleKind::Stabilizer => {
            widgets::gr_bar(ui, "Max cut", m[0], 12.0);
            widgets::gr_bar(ui, "Max boost", m[1], 12.0);
        }
        _ => return,
    }
    ui.add_space(6.0);
    let _ = Color32::WHITE;
}
