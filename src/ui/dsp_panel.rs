//! Editor for the built-in mastering suite.
//!
//! Left: the module chain in processing order. Click a module to edit it, click its box to switch
//! it on or off, and turn the mouse wheel over it to move it earlier or later in the chain.
//! Right: the selected module's parameters. Wheel adjusts the hovered parameter (Shift = fine),
//! left-click steps up, right-click steps down; toggles flip and choices cycle.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    FillRect, FrameRect, SelectObject, SetBkMode, SetTextColor, DT_CENTER, DT_LEFT, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, HDC, HGDIOBJ, TRANSPARENT,
};

use super::{draw_text_str, AppState, EXPORT_FORMATS, TARGETS};
use crate::dsp::{MasteringSuite, ModuleKind};

const CHAIN_LEFT: i32 = 20;
const CHAIN_RIGHT: i32 = 300;
const CHAIN_TOP: i32 = 72;
const CHAIN_ROW: i32 = 26;
const TOGGLE_RIGHT: i32 = CHAIN_LEFT + 44;

const PARAMS_LEFT: i32 = 320;
const PARAMS_TOP: i32 = 120;
const PARAM_ROW: i32 = 20;
const PARAM_COL_W: i32 = 296;
const PARAM_COL_GAP: i32 = 4;
const PARAM_ROWS_PER_COL: usize = 24;

const LEARN_BUTTON: RECT = RECT { left: 940, top: 72, right: 1220, bottom: 96 };
const AUTO_GAIN_BUTTON: RECT = RECT { left: 700, top: 72, right: 930, bottom: 96 };

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PanelHit {
    ChainToggle(ModuleKind),
    ChainSelect(ModuleKind),
    Param(usize),
    LearnMatch,
    AutoGain,
    SeparateStems,
}

fn inside(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn is_stem_module(kind: ModuleKind) -> bool {
    matches!(kind, ModuleKind::StemRebalance | ModuleKind::StemEq)
}

pub fn hit_test(suite: &MasteringSuite, selected: ModuleKind, x: i32, y: i32) -> Option<PanelHit> {
    if x >= CHAIN_LEFT && x < CHAIN_RIGHT && y >= CHAIN_TOP {
        let row = ((y - CHAIN_TOP) / CHAIN_ROW) as usize;
        let kind = *suite.chain.get(row)?;
        return Some(if x < TOGGLE_RIGHT { PanelHit::ChainToggle(kind) } else { PanelHit::ChainSelect(kind) });
    }
    if selected == ModuleKind::MatchEq && inside(&LEARN_BUTTON, x, y) {
        return Some(PanelHit::LearnMatch);
    }
    if is_stem_module(selected) && inside(&LEARN_BUTTON, x, y) {
        return Some(PanelHit::SeparateStems);
    }
    if inside(&AUTO_GAIN_BUTTON, x, y) {
        return Some(PanelHit::AutoGain);
    }
    if x >= PARAMS_LEFT && y >= PARAMS_TOP {
        let col = ((x - PARAMS_LEFT) / (PARAM_COL_W + PARAM_COL_GAP)) as usize;
        if (x - PARAMS_LEFT) % (PARAM_COL_W + PARAM_COL_GAP) >= PARAM_COL_W {
            return None;
        }
        let row = ((y - PARAMS_TOP) / PARAM_ROW) as usize;
        if row >= PARAM_ROWS_PER_COL {
            return None;
        }
        let index = col * PARAM_ROWS_PER_COL + row;
        if index < suite.module(selected).params().len() {
            return Some(PanelHit::Param(index));
        }
    }
    None
}

/// Whether a gesture changes the suite (as opposed to only the selection)
pub fn edits_suite(hit: PanelHit, steps: i32, is_wheel: bool) -> bool {
    match hit {
        PanelHit::ChainToggle(_) => is_wheel || steps > 0,
        PanelHit::ChainSelect(_) => is_wheel,
        PanelHit::Param(_) | PanelHit::AutoGain => true,
        PanelHit::LearnMatch | PanelHit::SeparateStems => false,
    }
}

/// Module selected after a gesture
pub fn selection_after(hit: PanelHit, current: ModuleKind, is_wheel: bool) -> ModuleKind {
    match hit {
        PanelHit::ChainToggle(kind) | PanelHit::ChainSelect(kind) if !is_wheel => kind,
        _ => current,
    }
}

/// Apply a gesture's change to a suite. Runs on both the UI copy and the audio thread's copy,
/// so it must only depend on its arguments.
pub fn apply_edit(suite: &mut MasteringSuite, selected: ModuleKind, hit: PanelHit, steps: i32, fine: bool, is_wheel: bool) {
    match hit {
        PanelHit::ChainToggle(kind) | PanelHit::ChainSelect(kind) if is_wheel => suite.move_module(kind, -steps),
        PanelHit::ChainToggle(kind) => {
            if steps > 0 {
                let m = suite.module_mut(kind);
                let on = m.enabled();
                m.set_enabled(!on);
            }
        }
        PanelHit::ChainSelect(_) => {}
        PanelHit::Param(i) => {
            let infos = suite.module(selected).params();
            if let Some(info) = infos.get(i) {
                let m = suite.module_mut(selected);
                let v = m.get_param(i);
                m.set_param(i, info.stepped(v, steps, fine));
            }
        }
        PanelHit::AutoGain => {
            let on = suite.auto_gain[selected.index()];
            suite.set_auto_gain(selected, !on);
        }
        PanelHit::LearnMatch | PanelHit::SeparateStems => {} // handled by the window
    }
}

struct ParamRow {
    label: String,
    value: String,
}

struct PanelData {
    chain: Vec<(ModuleKind, bool)>,
    rows: Vec<ParamRow>,
    selected_on: bool,
    auto_gain: bool,
    auto_gain_db: f32,
    status: Option<String>,
    latency: usize,
    sample_rate: f32,
}

fn gather(suite: &MasteringSuite, selected: ModuleKind) -> PanelData {
    let m = suite.module(selected);
    let rows = m
        .params()
        .iter()
        .enumerate()
        .map(|(i, info)| ParamRow { label: info.name.clone(), value: info.format(m.get_param(i)) })
        .collect();
    PanelData {
        chain: suite.chain.iter().map(|&k| (k, suite.module(k).enabled())).collect(),
        rows,
        selected_on: m.enabled(),
        auto_gain: suite.auto_gain[selected.index()],
        auto_gain_db: suite.auto_gain_db(selected),
        status: m.status(),
        latency: suite.latency_samples(),
        sample_rate: suite.sample_rate,
    }
}

pub unsafe fn paint(hdc: HDC, state: &AppState) {
    let selected = state.dsp_selected.get();
    let data = state.engine.with_dsp_suite(|s| gather(s, selected));

    let _ = SetBkMode(hdc, TRANSPARENT);
    let _ = SetTextColor(hdc, COLORREF(0x00E5FF));
    let _ = SelectObject(hdc, HGDIOBJ(state.hfont_title.0 as _));
    let mut title_rect = RECT { left: 20, top: 10, right: 1220, bottom: 40 };
    draw_text_str(hdc, "  Mastering Studio — Built-in Mastering Suite", &mut title_rect, DT_LEFT | DT_SINGLELINE | DT_VCENTER);

    let _ = SetTextColor(hdc, COLORREF(0x777777));
    let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));
    let mut hint_rect = RECT { left: 20, top: 40, right: 1220, bottom: 64 };
    draw_text_str(
        hdc,
        "  Chain: click a module to edit, click its box to switch it on/off, wheel to reorder  •  Parameters: wheel (Shift = fine), left-click +, right-click −",
        &mut hint_rect,
        DT_LEFT | DT_SINGLELINE | DT_VCENTER,
    );

    // Module chain
    let chain_rect = RECT { left: CHAIN_LEFT - 2, top: CHAIN_TOP - 2, right: CHAIN_RIGHT + 2, bottom: CHAIN_TOP + data.chain.len() as i32 * CHAIN_ROW + 2 };
    let _ = FillRect(hdc, &chain_rect, state.panel_brush);
    let _ = FrameRect(hdc, &chain_rect, state.meter_dim_brush);
    for (row, (kind, on)) in data.chain.iter().enumerate() {
        let y = CHAIN_TOP + row as i32 * CHAIN_ROW;
        let row_rect = RECT { left: CHAIN_LEFT, top: y, right: CHAIN_RIGHT, bottom: y + CHAIN_ROW - 2 };
        let hovered = matches!(state.dsp_hover.get(), Some(PanelHit::ChainSelect(k)) | Some(PanelHit::ChainToggle(k)) if k == *kind);
        if *kind == selected {
            let _ = FillRect(hdc, &row_rect, state.corr_dim_brush);
            let _ = FrameRect(hdc, &row_rect, state.accent_brush);
        } else if hovered {
            let _ = FillRect(hdc, &row_rect, state.corr_dim_brush);
        }

        let toggle_rect = RECT { left: CHAIN_LEFT + 6, top: y + 4, right: TOGGLE_RIGHT - 6, bottom: y + CHAIN_ROW - 6 };
        let _ = FillRect(hdc, &toggle_rect, if *on { state.meter_green_brush } else { state.meter_dim_brush });
        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_mono.0 as _));
        let _ = SetTextColor(hdc, if *on { COLORREF(0x101010) } else { COLORREF(0x888888) });
        let mut tr = toggle_rect;
        draw_text_str(hdc, if *on { "ON" } else { "OFF" }, &mut tr, DT_CENTER | DT_SINGLELINE | DT_VCENTER);

        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));
        let _ = SetTextColor(hdc, if *on { COLORREF(0xEEEEEE) } else { COLORREF(0x888888) });
        let mut name_rect = RECT { left: TOGGLE_RIGHT + 4, top: y, right: CHAIN_RIGHT - 6, bottom: y + CHAIN_ROW - 2 };
        draw_text_str(hdc, &format!("{:>2}. {}", row + 1, kind.name()), &mut name_rect, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    }

    // Chain summary
    let info_top = CHAIN_TOP + data.chain.len() as i32 * CHAIN_ROW + 8;
    let latency_ms = data.latency as f32 / data.sample_rate.max(1.0) * 1000.0;
    let lines = [
        format!("Latency: {} samples ({:.1} ms)", data.latency, latency_ms),
        TARGETS[state.current_target_idx].name.to_string(),
        format!("Export: {}", EXPORT_FORMATS[state.export_format_idx].name),
    ];
    let _ = SetTextColor(hdc, COLORREF(0xAAAAAA));
    for (i, line) in lines.iter().enumerate() {
        let mut r = RECT { left: CHAIN_LEFT, top: info_top + i as i32 * 18, right: 600, bottom: info_top + (i as i32 + 1) * 18 };
        draw_text_str(hdc, line, &mut r, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    }

    // Selected module header
    let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
    let _ = SetTextColor(hdc, if data.selected_on { COLORREF(0x00E5FF) } else { COLORREF(0x888888) });
    let mut head_rect = RECT { left: PARAMS_LEFT, top: 72, right: AUTO_GAIN_BUTTON.left - 8, bottom: 96 };
    let head = format!("{}{}", selected.name(), if data.selected_on { "" } else { "  (off: click its box to enable)" });
    draw_text_str(hdc, &head, &mut head_rect, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    if let Some(ref status) = data.status {
        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));
        let _ = SetTextColor(hdc, COLORREF(0x00FF88));
        let mut st = RECT { left: PARAMS_LEFT, top: 96, right: 1220, bottom: 116 };
        draw_text_str(hdc, status, &mut st, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    }
    {
        let hovered = state.dsp_hover.get() == Some(PanelHit::AutoGain);
        let _ = FillRect(hdc, &AUTO_GAIN_BUTTON, if data.auto_gain { state.meter_green_brush } else if hovered { state.corr_dim_brush } else { state.panel_brush });
        let _ = FrameRect(hdc, &AUTO_GAIN_BUTTON, state.meter_dim_brush);
        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
        let _ = SetTextColor(hdc, if data.auto_gain { COLORREF(0x101010) } else { COLORREF(0xAAAAAA) });
        let label = if data.auto_gain {
            format!("Auto gain-match: ON ({:+.1} dB)", data.auto_gain_db)
        } else {
            "Auto gain-match: OFF".to_string()
        };
        let mut br = AUTO_GAIN_BUTTON;
        draw_text_str(hdc, &label, &mut br, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
    }
    if is_stem_module(selected) {
        use crate::audio::engine::StemStatus;
        let status = state.engine.stem_status();
        let (button, line) = match status {
            StemStatus::Unavailable => (None, "Real-time approximation (no trained model found in models/)".to_string()),
            StemStatus::NotSeparated => (Some("Separate stems (AI)"), "Real-time approximation: separate the track for full quality".to_string()),
            StemStatus::Separating(p) => (None, format!("Separating stems with the trained model: {:.0}%", p * 100.0)),
            StemStatus::Ready => (Some("Re-separate stems (AI)"), "Using AI-separated stems (playback and export)".to_string()),
        };
        if let Some(label) = button {
            let hovered = state.dsp_hover.get() == Some(PanelHit::SeparateStems);
            let _ = FillRect(hdc, &LEARN_BUTTON, if hovered { state.corr_dim_brush } else { state.panel_brush });
            let _ = FrameRect(hdc, &LEARN_BUTTON, state.accent_brush);
            let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
            let _ = SetTextColor(hdc, COLORREF(0x00E5FF));
            let mut br = LEARN_BUTTON;
            draw_text_str(hdc, label, &mut br, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
        }
        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));
        let _ = SetTextColor(hdc, if status == StemStatus::Ready { COLORREF(0x00FF88) } else { COLORREF(0x00D7FF) });
        let mut st = RECT { left: PARAMS_LEFT, top: 96, right: 1220, bottom: 116 };
        draw_text_str(hdc, &line, &mut st, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    }
    if selected == ModuleKind::MatchEq {
        let hovered = state.dsp_hover.get() == Some(PanelHit::LearnMatch);
        let _ = FillRect(hdc, &LEARN_BUTTON, if hovered { state.corr_dim_brush } else { state.panel_brush });
        let _ = FrameRect(hdc, &LEARN_BUTTON, state.accent_brush);
        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
        let _ = SetTextColor(hdc, COLORREF(0x00E5FF));
        let mut br = LEARN_BUTTON;
        draw_text_str(hdc, "Learn from reference track", &mut br, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
    }

    // Parameter grid
    let cols = (data.rows.len() + PARAM_ROWS_PER_COL - 1) / PARAM_ROWS_PER_COL;
    for c in 0..cols.max(1) {
        let x = PARAMS_LEFT + c as i32 * (PARAM_COL_W + PARAM_COL_GAP);
        let rows_here = data.rows.len().saturating_sub(c * PARAM_ROWS_PER_COL).min(PARAM_ROWS_PER_COL);
        let col_rect = RECT { left: x - 2, top: PARAMS_TOP - 2, right: x + PARAM_COL_W + 2, bottom: PARAMS_TOP + rows_here.max(1) as i32 * PARAM_ROW + 2 };
        let _ = FillRect(hdc, &col_rect, state.panel_brush);
        let _ = FrameRect(hdc, &col_rect, state.meter_dim_brush);
    }
    for (i, row) in data.rows.iter().enumerate() {
        let col = i / PARAM_ROWS_PER_COL;
        let x = PARAMS_LEFT + col as i32 * (PARAM_COL_W + PARAM_COL_GAP);
        let y = PARAMS_TOP + (i % PARAM_ROWS_PER_COL) as i32 * PARAM_ROW;
        if state.dsp_hover.get() == Some(PanelHit::Param(i)) {
            let hr = RECT { left: x, top: y, right: x + PARAM_COL_W, bottom: y + PARAM_ROW };
            let _ = FillRect(hdc, &hr, state.corr_dim_brush);
        }
        let mut lr = RECT { left: x + 8, top: y, right: x + PARAM_COL_W - 8, bottom: y + PARAM_ROW };
        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));
        let _ = SetTextColor(hdc, if data.selected_on { COLORREF(0xDDDDDD) } else { COLORREF(0x777777) });
        draw_text_str(hdc, &row.label, &mut lr, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_mono.0 as _));
        let _ = SetTextColor(hdc, if data.selected_on { COLORREF(0x00FF88) } else { COLORREF(0x777777) });
        let mut vr = lr;
        draw_text_str(hdc, &row.value, &mut vr, DT_RIGHT | DT_SINGLELINE | DT_VCENTER);
    }
}
