//! Custom-drawn widgets: level meters, waveform scrubber, EQ curve, gain-reduction bars

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};

use crate::audio::{AudioBuffer, MeterLevels, TrimFadeSettings};
use crate::dsp::{DynamicEqualizer, EqFilterType};

pub const ACCENT: Color32 = Color32::from_rgb(0, 200, 230);
pub const GOOD: Color32 = Color32::from_rgb(70, 220, 130);
pub const WARN: Color32 = Color32::from_rgb(240, 190, 60);
pub const HOT: Color32 = Color32::from_rgb(240, 80, 80);
pub const PANEL: Color32 = Color32::from_rgb(28, 30, 36);
pub const GRID: Color32 = Color32::from_rgb(52, 56, 66);
pub const DIM: Color32 = Color32::from_rgb(120, 126, 140);

#[inline]
fn stroke(width: f32, color: Color32) -> Stroke {
    Stroke::new(width, color)
}

fn db_to_frac(db: f32, floor: f32) -> f32 {
    ((db - floor) / -floor).clamp(0.0, 1.0)
}

/// Stereo peak/RMS bars with clip indicator, plus a phase-correlation bar
pub fn level_meters(ui: &mut egui::Ui, m: &MeterLevels, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, PANEL);
    let floor = -60.0;
    let bar_h = (rect.height() - 14.0) / 2.0;
    for (row, (peak, rms)) in [(m.peak_l_db, m.rms_l_db), (m.peak_r_db, m.rms_r_db)].into_iter().enumerate() {
        let y = rect.top() + 3.0 + row as f32 * (bar_h + 2.0);
        let x0 = rect.left() + 14.0;
        let w = rect.width() - 30.0;
        let track = Rect::from_min_size(Pos2::new(x0, y), Vec2::new(w, bar_h));
        p.rect_filled(track, 1.0, GRID);
        let rms_w = w * db_to_frac(rms, floor);
        let peak_x = x0 + w * db_to_frac(peak, floor);
        let color = if peak > -0.1 { HOT } else if peak > -6.0 { WARN } else { GOOD };
        p.rect_filled(Rect::from_min_size(track.min, Vec2::new(rms_w, bar_h)), 1.0, color.gamma_multiply(0.75));
        p.line_segment([Pos2::new(peak_x, y), Pos2::new(peak_x, y + bar_h)], stroke(2.0, color));
        p.text(Pos2::new(rect.left() + 3.0, y + bar_h / 2.0), Align2::LEFT_CENTER, if row == 0 { "L" } else { "R" }, FontId::monospace(10.0), DIM);
        let clip = Rect::from_min_size(Pos2::new(rect.right() - 12.0, y), Vec2::new(9.0, bar_h));
        p.rect_filled(clip, 1.0, if peak > -0.1 { HOT } else { GRID });
    }
    // Correlation: -1 (left) .. +1 (right)
    let y = rect.bottom() - 7.0;
    let x0 = rect.left() + 14.0;
    let w = rect.width() - 30.0;
    p.line_segment([Pos2::new(x0, y), Pos2::new(x0 + w, y)], stroke(3.0, GRID));
    let cx = x0 + w * ((m.phase_correlation + 1.0) / 2.0).clamp(0.0, 1.0);
    let color = if m.phase_correlation < 0.0 { HOT } else { GOOD };
    p.line_segment([Pos2::new(x0 + w / 2.0, y), Pos2::new(cx, y)], stroke(3.0, color));
}

/// What the user did on the waveform
pub enum WaveformAction {
    None,
    Seek(f64),
    TrimIn(f64),
    TrimOut(f64),
}

/// Stereo waveform overview with playhead and trim region. Click/drag seeks, Shift+click sets
/// the trim in point, right-click the trim out point.
pub fn waveform(ui: &mut egui::Ui, buffer: Option<&AudioBuffer>, position: f64, trim: &TrimFadeSettings, referencing: bool, size: Vec2) -> WaveformAction {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, PANEL);
    let Some(buf) = buffer else {
        p.text(rect.center(), Align2::CENTER_CENTER, "Drop or open an audio file", FontId::proportional(13.0), DIM);
        return WaveformAction::None;
    };
    let peaks = buf.peaks();
    let mid_l = rect.top() + rect.height() * 0.25;
    let mid_r = rect.top() + rect.height() * 0.75;
    let half = rect.height() * 0.23;
    let color = if referencing { WARN } else { ACCENT };
    if !peaks.is_empty() {
        let n = peaks.len();
        for (i, pk) in peaks.iter().enumerate() {
            let x = rect.left() + rect.width() * (i as f32 + 0.5) / n as f32;
            let lc = if pk.max_l.max(-pk.min_l) > 0.98 { WARN } else { color };
            let rc = if pk.max_r.max(-pk.min_r) > 0.98 { WARN } else { color };
            p.line_segment([Pos2::new(x, mid_l - pk.max_l * half), Pos2::new(x, mid_l - pk.min_l * half)], stroke(1.5, lc));
            p.line_segment([Pos2::new(x, mid_r - pk.max_r * half), Pos2::new(x, mid_r - pk.min_r * half)], stroke(1.5, rc));
        }
    }
    // Trimmed-off regions
    let shade = Color32::from_black_alpha(150);
    let in_x = rect.left() + rect.width() * trim.trim_start_fraction as f32;
    let out_x = rect.left() + rect.width() * trim.trim_end_fraction as f32;
    if trim.trim_start_fraction > 0.0005 {
        p.rect_filled(Rect::from_min_max(rect.min, Pos2::new(in_x, rect.bottom())), 0.0, shade);
        p.line_segment([Pos2::new(in_x, rect.top()), Pos2::new(in_x, rect.bottom())], stroke(1.5, WARN));
    }
    if trim.trim_end_fraction < 0.9995 {
        p.rect_filled(Rect::from_min_max(Pos2::new(out_x, rect.top()), rect.max), 0.0, shade);
        p.line_segment([Pos2::new(out_x, rect.top()), Pos2::new(out_x, rect.bottom())], stroke(1.5, WARN));
    }
    let x = rect.left() + rect.width() * position.clamp(0.0, 1.0) as f32;
    p.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], stroke(2.0, Color32::WHITE));

    let frac = |pos: Pos2| ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
    if resp.secondary_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            return WaveformAction::TrimOut(frac(pos));
        }
    }
    if resp.clicked() || resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if ui.input(|i| i.modifiers.shift) {
                return if resp.clicked() { WaveformAction::TrimIn(frac(pos)) } else { WaveformAction::None };
            }
            return WaveformAction::Seek(frac(pos));
        }
    }
    WaveformAction::None
}

/// Horizontal gain-reduction / amount bar: 0 at the left, `max_db` at the right
pub fn gr_bar(ui: &mut egui::Ui, label: &str, db: f32, max_db: f32) {
    ui.horizontal(|ui| {
        ui.add_sized([110.0, 16.0], egui::Label::new(egui::RichText::new(label).color(DIM).size(12.0)));
        let (rect, _) = ui.allocate_exact_size(Vec2::new(220.0, 12.0), Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 2.0, GRID);
        let frac = (db.abs() / max_db).clamp(0.0, 1.0);
        p.rect_filled(Rect::from_min_size(rect.min, Vec2::new(rect.width() * frac, rect.height())), 2.0, WARN);
        ui.label(egui::RichText::new(format!("{:.1} dB", db.abs())).monospace().size(12.0));
    });
}

const F_MIN: f32 = 20.0;
const F_MAX: f32 = 20000.0;
const EQ_RANGE_DB: f32 = 18.0;

fn freq_to_x(f: f32, rect: Rect) -> f32 {
    rect.left() + rect.width() * ((f.max(F_MIN) / F_MIN).ln() / (F_MAX / F_MIN).ln())
}

fn x_to_freq(x: f32, rect: Rect) -> f32 {
    F_MIN * (F_MAX / F_MIN).powf(((x - rect.left()) / rect.width()).clamp(0.0, 1.0))
}

fn db_to_y(db: f32, rect: Rect) -> f32 {
    rect.center().y - db / EQ_RANGE_DB * rect.height() * 0.5
}

fn y_to_db(y: f32, rect: Rect) -> f32 {
    ((rect.center().y - y) / (rect.height() * 0.5) * EQ_RANGE_DB).clamp(-EQ_RANGE_DB, EQ_RANGE_DB)
}

/// A change made on the EQ curve: (band, new frequency, new gain, new Q)
pub struct EqEdit {
    pub band: usize,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
}

/// Interactive EQ: live spectrum, each band's node and the combined response. Drag a node to
/// move frequency (and gain for shelves/bells); scroll over it to change Q. Returns the edit and
/// the band under the pointer.
pub fn eq_curve(
    ui: &mut egui::Ui,
    eq: &DynamicEqualizer,
    spectrum_db: &[f32; 128],
    spectrum_top_hz: f32,
    selected_band: &mut Option<usize>,
    size: Vec2,
) -> Option<EqEdit> {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, PANEL);

    // Grid
    for f in [30.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0] {
        let x = freq_to_x(f, rect);
        p.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], stroke(1.0, GRID));
        let label = if f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{}", f) };
        p.text(Pos2::new(x + 2.0, rect.bottom() - 2.0), Align2::LEFT_BOTTOM, label, FontId::monospace(10.0), DIM);
    }
    for db in [-12.0, -6.0, 0.0, 6.0, 12.0] {
        let y = db_to_y(db, rect);
        p.line_segment([Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)], stroke(if db == 0.0 { 1.5 } else { 1.0 }, GRID));
        p.text(Pos2::new(rect.left() + 3.0, y - 1.0), Align2::LEFT_BOTTOM, format!("{:+}", db), FontId::monospace(10.0), DIM);
    }

    // Live spectrum (128 log bins from 20 Hz to `spectrum_top_hz`), drawn as a filled area
    let mut spec_pts = Vec::with_capacity(130);
    for (i, &db) in spectrum_db.iter().enumerate() {
        let f = F_MIN * (spectrum_top_hz / F_MIN).powf(i as f32 / 127.0);
        let y = rect.bottom() - rect.height() * ((db + 90.0) / 90.0).clamp(0.0, 1.0);
        spec_pts.push(Pos2::new(freq_to_x(f, rect), y));
    }
    let spec_color = Color32::from_rgba_unmultiplied(0, 200, 230, 40);
    if spec_pts.len() > 1 {
        for w in spec_pts.windows(2) {
            p.add(Shape::convex_polygon(
                vec![w[0], w[1], Pos2::new(w[1].x, rect.bottom()), Pos2::new(w[0].x, rect.bottom())],
                spec_color,
                Stroke::NONE,
            ));
        }
    }

    // Combined response
    let enabled = eq.enabled;
    let steps = (rect.width() as usize).max(2);
    let curve: Vec<Pos2> = (0..steps)
        .map(|i| {
            let x = rect.left() + rect.width() * i as f32 / (steps - 1) as f32;
            let f = x_to_freq(x, rect);
            let db: f32 = if enabled { eq.bands.iter().map(|b| b.response_db(f)).sum() } else { 0.0 };
            Pos2::new(x, db_to_y(db.clamp(-EQ_RANGE_DB, EQ_RANGE_DB), rect))
        })
        .collect();
    p.add(Shape::line(curve, stroke(2.5, if enabled { ACCENT } else { DIM })));

    // Band nodes
    let node_pos = |i: usize| -> Pos2 {
        let b = &eq.bands[i];
        let has_gain = !matches!(b.filter_type, EqFilterType::HighPass | EqFilterType::LowPass);
        Pos2::new(freq_to_x(b.freq, rect), db_to_y(if has_gain { b.static_gain_db } else { 0.0 }, rect))
    };
    let pointer = resp.hover_pos();
    let near = |pos: Pos2| -> Option<usize> {
        (0..eq.bands.len())
            .map(|i| (i, node_pos(i).distance(pos)))
            .filter(|(_, d)| *d < 14.0)
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .map(|(i, _)| i)
    };
    let hovered = pointer.and_then(near);
    for i in 0..eq.bands.len() {
        let b = &eq.bands[i];
        let pos = node_pos(i);
        let active = *selected_band == Some(i) || hovered == Some(i);
        let fill = if !b.enabled { GRID } else if active { Color32::WHITE } else { ACCENT };
        p.circle_filled(pos, if active { 8.0 } else { 6.0 }, fill);
        p.text(pos + Vec2::new(0.0, -14.0), Align2::CENTER_BOTTOM, format!("{}", i + 1), FontId::proportional(11.0), if b.enabled { Color32::WHITE } else { DIM });
    }

    if resp.drag_started() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            *selected_band = near(pos).or(*selected_band);
        }
    }
    let mut edit = None;
    if resp.dragged() {
        if let (Some(i), Some(pos)) = (*selected_band, resp.interact_pointer_pos()) {
            let b = &eq.bands[i];
            let has_gain = !matches!(b.filter_type, EqFilterType::HighPass | EqFilterType::LowPass);
            edit = Some(EqEdit {
                band: i,
                freq: x_to_freq(pos.x, rect).clamp(20.0, 20000.0),
                gain_db: if has_gain { y_to_db(pos.y, rect) } else { b.static_gain_db },
                q: b.q,
            });
        }
    }
    if let Some(i) = hovered {
        let scroll = ui.input(|inp| inp.raw_scroll_delta.y);
        if scroll.abs() > 0.0 {
            let b = &eq.bands[i];
            edit = Some(EqEdit { band: i, freq: b.freq, gain_db: b.static_gain_db, q: (b.q * (1.0 + scroll.signum() * 0.08)).clamp(0.2, 10.0) });
            *selected_band = Some(i);
        }
    }
    edit
}
