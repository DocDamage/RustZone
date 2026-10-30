//! Stem mixer: separate a track (trained model when available) or load stem files, then remix

use eframe::egui::{self, RichText};

use super::app::MasteringApp;
use super::widgets::{DIM, GOOD, WARN};
use super::AUDIO_EXTENSIONS;
use crate::audio::{StemStatus, NUM_STEMS};

pub fn show(app: &mut MasteringApp, ui: &mut egui::Ui) {
    let busy = app.busy.load(std::sync::atomic::Ordering::Relaxed);
    ui.horizontal(|ui| {
        let ai = !matches!(app.engine.stem_status(), StemStatus::Unavailable);
        let label = if ai { "Separate track (AI model)" } else { "Separate track (spectral)" };
        if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
            let ctx = app.ctx.clone();
            app.spawn_job(move |engine, status| {
                let res = engine.separate_active_track_into_stems(move |p, msg| {
                    let _ = (p, msg);
                    ctx.request_repaint();
                });
                match res {
                    Ok(()) => status.set("Stems loaded into the mixer"),
                    Err(e) => status.set(format!("Separation failed: {}", e)),
                }
            });
        }
        if ui.button("Export all stems…").clicked() {
            if let Some(folder) = rfd::FileDialog::new().set_title("Export stems to folder").pick_folder() {
                match app.engine.with_stem_mixer(|sm| sm.export_all_stems(&folder, "stem")) {
                    Ok(files) => app.status.set(format!("Exported {} stems to {}", files.len(), folder.display())),
                    Err(e) => app.status.set(e),
                }
            }
        }
        if ui.button("Clear").clicked() {
            app.engine.with_stem_mixer(|sm| sm.clear_all());
        }
        if let StemStatus::Separating(p) = app.engine.stem_status() {
            ui.spinner();
            ui.label(RichText::new(format!("Separating: {:.0}%", p * 100.0)).color(WARN));
        }
    });
    ui.label(RichText::new("Changes re-sum the stems into the playing track without moving the playhead.").color(DIM).size(11.0));
    ui.separator();

    let stems = app.engine.with_stem_mixer(|sm| sm.stems.clone());
    let mut changed = false;
    let mut load: Option<usize> = None;
    ui.columns(NUM_STEMS, |cols| {
        for (i, col) in cols.iter_mut().enumerate() {
            let s = &stems[i];
            col.vertical(|ui| {
                ui.label(RichText::new(&s.name).strong());
                let loaded = s.buffer.is_some();
                ui.label(RichText::new(if loaded { "loaded" } else { "empty" }).color(if loaded { GOOD } else { DIM }).size(11.0));
                if ui.button("Load file…").clicked() {
                    load = Some(i);
                }
                ui.add_space(4.0);
                let mut gain = s.gain_db;
                ui.label("Gain");
                if ui.add(egui::Slider::new(&mut gain, -24.0..=12.0).suffix(" dB")).changed() {
                    app.engine.with_stem_mixer(|sm| sm.set_stem_gain(i, gain));
                    changed = true;
                }
                let mut pan = s.pan;
                ui.label("Pan");
                if ui.add(egui::Slider::new(&mut pan, -1.0..=1.0)).changed() {
                    app.engine.with_stem_mixer(|sm| sm.set_stem_pan(i, pan));
                    changed = true;
                }
                ui.horizontal(|ui| {
                    if ui.selectable_label(s.mute, "Mute").clicked() {
                        app.engine.with_stem_mixer(|sm| sm.toggle_stem_mute(i));
                        changed = true;
                    }
                    if ui.selectable_label(s.solo, "Solo").clicked() {
                        app.engine.with_stem_mixer(|sm| sm.toggle_stem_solo(i));
                        changed = true;
                    }
                });
            });
        }
    });
    if let Some(i) = load {
        if let Some(path) = rfd::FileDialog::new().set_title("Load stem").add_filter("Audio", &AUDIO_EXTENSIONS).pick_file() {
            match app.engine.with_stem_mixer(|sm| sm.stems[i].load_file(path)) {
                Ok(()) => changed = true,
                Err(e) => app.status.set(e),
            }
        }
    }
    if changed {
        if let Err(e) = app.engine.update_master_from_stems() {
            app.status.set(e);
        }
    }
}
