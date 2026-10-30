//! Album montage: sequence tracks, balance loudness, play the album, export WAV + CUE

use eframe::egui::{self, RichText};

use super::app::{with_render_source, MasteringApp};
use super::widgets::{DIM, GOOD, HOT};
use super::{AUDIO_EXTENSIONS, EXPORT_FORMATS};

pub fn show(app: &mut MasteringApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        if ui.button("+ Add tracks…").clicked() {
            if let Some(files) = rfd::FileDialog::new().set_title("Add album tracks").add_filter("Audio", &AUDIO_EXTENSIONS).pick_files() {
                app.add_album_tracks(files);
            }
        }
        if ui.button("Balance loudness").on_hover_text("Gain every track to the album target").clicked() {
            app.engine.with_montage(|m| m.auto_balance_album_lufs());
            app.refresh_album_playback();
        }
        let busy = app.busy.load(std::sync::atomic::Ordering::Relaxed);
        if ui.add_enabled(!busy, egui::Button::new("Export album (WAV + CUE)…")).clicked() {
            export_album(app);
        }
        if ui.button("Clear").clicked() {
            app.engine.with_montage(|m| m.tracks.clear());
            app.refresh_album_playback();
        }
        ui.separator();
        let mut target = app.engine.with_montage(|m| m.target_album_lufs);
        ui.label("Album target");
        if ui.add(egui::DragValue::new(&mut target).range(-30.0..=-6.0).speed(0.1).suffix(" LUFS")).changed() {
            app.engine.with_montage(|m| m.target_album_lufs = target);
        }
    });
    ui.label(RichText::new("This tab plays the whole album through the mastering chain. Drop files here to add them.").color(DIM).size(11.0));
    ui.separator();

    let tracks = app.engine.with_montage(|m| m.tracks.clone());
    let mut changed = false;
    let mut remove = None;
    let mut move_up = None;
    let mut edits: Vec<(usize, f32, f64, bool)> = Vec::new();
    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("album").striped(true).num_columns(9).spacing([14.0, 6.0]).show(ui, |ui| {
            for h in ["#", "Title", "Length", "Loudness", "True peak", "Gain", "Gap before", "Play", ""] {
                ui.label(RichText::new(h).color(DIM).strong());
            }
            ui.end_row();
            for (i, t) in tracks.iter().enumerate() {
                ui.label(format!("{:02}", i + 1));
                ui.label(&t.title);
                ui.label(format!("{:02}:{:02}", (t.duration_seconds / 60.0) as u32, (t.duration_seconds % 60.0) as u32));
                ui.label(format!("{:.1} LUFS", t.integrated_lufs));
                ui.label(RichText::new(format!("{:.1} dBTP", t.true_peak_dbtp)).color(if t.true_peak_dbtp > -0.5 { HOT } else { GOOD }));
                let mut gain = t.gain_offset_db;
                let mut gap = t.pause_gap_seconds;
                let mut active = t.is_active;
                let g = ui.add(egui::DragValue::new(&mut gain).range(-12.0..=6.0).speed(0.1).suffix(" dB")).changed();
                let p = ui.add(egui::DragValue::new(&mut gap).range(0.0..=10.0).speed(0.1).suffix(" s")).changed();
                let a = ui.checkbox(&mut active, "").changed();
                if g || p || a {
                    edits.push((i, gain, gap, active));
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(i > 0, egui::Button::new("⏶").small()).clicked() {
                        move_up = Some(i);
                    }
                    if ui.button("✕").on_hover_text("Remove").clicked() {
                        remove = Some(t.id);
                    }
                });
                ui.end_row();
            }
        });
        if tracks.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new("The album is empty. Add or drop tracks to assemble it.").color(DIM));
        }
    });
    if !edits.is_empty() {
        app.engine.with_montage(|m| {
            for &(i, gain, gap, active) in &edits {
                if let Some(t) = m.tracks.get_mut(i) {
                    t.gain_offset_db = gain;
                    t.pause_gap_seconds = gap;
                    t.is_active = active;
                }
            }
        });
        changed = true;
    }
    if let Some(i) = move_up {
        app.engine.with_montage(|m| m.move_track(i, i - 1));
        changed = true;
    }
    if let Some(id) = remove {
        app.engine.with_montage(|m| m.remove_track(id));
        changed = true;
    }
    if changed {
        app.refresh_album_playback();
    }

    ui.separator();
    let (total, active) = app.engine.with_montage(|m| (m.total_duration_seconds(), m.tracks.iter().filter(|t| t.is_active).count()));
    let cd_limit = 79.0 * 60.0 + 57.0;
    ui.label(format!(
        "{} active tracks • {:02}:{:02} total • {:.0}% of an 80-minute CD",
        active,
        (total / 60.0) as u32,
        (total % 60.0) as u32,
        (total / cd_limit * 100.0).min(100.0)
    ));
}

fn export_album(app: &mut MasteringApp) {
    let format = EXPORT_FORMATS[app.export_format];
    let Some(path) = rfd::FileDialog::new()
        .set_title(format!("Export album ({})", format.name))
        .set_file_name("album_master.wav")
        .add_filter("WAV", &["wav"])
        .save_file()
    else {
        return;
    };
    app.engine.pause();
    let montage = app.engine.with_montage(|m| m.clone());
    let mode = app.engine.processing_mode();
    let ctx = app.ctx.clone();
    app.spawn_job(move |engine, status| {
        let res = with_render_source(engine, mode, |source| {
            montage.export_album_with_source(&path, format.bit_depth, format.sample_rate, source, |p, msg| {
                status.set(format!("Album {:.0}%: {}", p * 100.0, msg));
                ctx.request_repaint();
            })
        });
        match res {
            Ok(cue) => status.set(format!("Album exported with CUE sheet {}", cue.display())),
            Err(e) => status.set(format!("Album export failed: {}", e)),
        }
    });
}
