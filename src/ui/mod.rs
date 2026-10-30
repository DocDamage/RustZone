#![allow(static_mut_refs)]

mod dsp_panel;

use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
    FillRect, FrameRect, InvalidateRect, Polyline, ScreenToClient, SelectObject, SetBkColor, SetBkMode, SetTextColor, UpdateWindow,
    DRAW_TEXT_FORMAT, DT_CENTER, DT_LEFT, DT_SINGLELINE, DT_VCENTER, HBRUSH, HDC, HFONT, HGDIOBJ, HPEN, PAINTSTRUCT,
    PS_SOLID, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, INITCOMMONCONTROLSEX, ICC_BAR_CLASSES, ICC_STANDARD_CLASSES,
};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;

#[link(name = "user32")]
extern "system" {
    fn GetKeyState(nVirtKey: i32) -> i16;
}

use crate::audio::{
    load_audio_file, render_to_wav, AudioEngine, BwfMetadata, ExportBitDepth,
    FadeShape, ProcessingMode, RenderOptions, RenderSource,
};
use crate::dsp::{calculate_buffer_integrated_lufs, ModuleKind, XmlPreset};
use std::cell::Cell;
use crate::midi::{MidiController, WM_APP_MIDI_CC, WM_APP_MIDI_NOTE};
use crate::vst3::Vst3Plugin;

const TIMER_ID_REFRESH: usize = 1001;

/// Size of the central viewport when it shows the built-in panels instead of a plugin editor
const VIEWPORT_W: i32 = 1240;
const VIEWPORT_H: i32 = 625;

// Control IDs
const ID_BTN_OPEN: usize = 201;
const ID_BTN_EXPORT: usize = 202;
const ID_COMBO_MODULE: usize = 203;
const ID_BTN_BYPASS: usize = 204;
const ID_BTN_MODE: usize = 205;
const ID_BTN_PLAY: usize = 206;
const ID_BTN_PAUSE: usize = 207;
const ID_BTN_STOP: usize = 208;
const ID_BTN_LOOP: usize = 209;
const ID_SLIDER_TRACK: usize = 210;
const ID_LABEL_TIME: usize = 211;
const ID_LABEL_METER: usize = 212;
const ID_LABEL_INFO: usize = 213;
const ID_BTN_SAVE_PRESET: usize = 214;
const ID_BTN_LOAD_PRESET: usize = 215;
const ID_METER_CANVAS: usize = 216;
const ID_BTN_LOAD_REF: usize = 217;
const ID_BTN_TOGGLE_REF: usize = 218;
const ID_BTN_MATCH_GAIN: usize = 219;
const ID_BTN_TARGET: usize = 220;
const ID_BTN_BATCH: usize = 221;
const ID_BTN_MONTAGE: usize = 222;
const ID_BTN_STEMS: usize = 223;
const ID_BTN_DEVICE_SETTINGS: usize = 224;

#[derive(Clone, Copy)]
struct LoudnessTarget {
    name: &'static str,
    target_lufs: f32,
    ceiling_dbtp: f32,
}

#[derive(Clone, Copy)]
struct ExportFormat {
    name: &'static str,
    bit_depth: ExportBitDepth,
    sample_rate: Option<u32>,
}

/// Export formats cycled with the 'D' key. Integer formats are dithered by the renderer.
const EXPORT_FORMATS: [ExportFormat; 4] = [
    ExportFormat { name: "24-bit WAV, native rate (TPDF dither)", bit_depth: ExportBitDepth::Bit24, sample_rate: None },
    ExportFormat { name: "16-bit / 44.1 kHz CD WAV (noise-shaped dither)", bit_depth: ExportBitDepth::Bit16, sample_rate: Some(44100) },
    ExportFormat { name: "32-bit float WAV, native rate", bit_depth: ExportBitDepth::Float32, sample_rate: None },
    ExportFormat { name: "24-bit / 48 kHz WAV for video (TPDF dither)", bit_depth: ExportBitDepth::Bit24, sample_rate: Some(48000) },
];

const TARGETS: [LoudnessTarget; 5] = [
    LoudnessTarget { name: "Target: Spotify (-14 LUFS)", target_lufs: -14.0, ceiling_dbtp: -1.0 },
    LoudnessTarget { name: "Target: Apple (-16 LUFS)", target_lufs: -16.0, ceiling_dbtp: -1.0 },
    LoudnessTarget { name: "Target: YouTube (-14 LUFS)", target_lufs: -14.0, ceiling_dbtp: -1.0 },
    LoudnessTarget { name: "Target: CD/Club (-9 LUFS)", target_lufs: -9.0, ceiling_dbtp: -0.1 },
    LoudnessTarget { name: "Target: Broadcast (-23 LUFS)", target_lufs: -23.0, ceiling_dbtp: -1.0 },
];

struct AppState {
    engine: Arc<AudioEngine>,
    vst3_plugins: Vec<(String, PathBuf)>,
    selected_module_idx: usize,
    child_viewport: HWND,
    waveform_canvas_hwnd: HWND,
    time_label_hwnd: HWND,
    meter_label_hwnd: HWND,
    meter_canvas_hwnd: HWND,
    info_label_hwnd: HWND,
    bypass_btn_hwnd: HWND,
    loop_btn_hwnd: HWND,
    mode_btn_hwnd: HWND,
    ref_toggle_btn_hwnd: HWND,
    target_btn_hwnd: HWND,
    montage_btn_hwnd: HWND,
    stems_btn_hwnd: HWND,
    in_stems_mode: bool,
    current_target_idx: usize,
    export_format_idx: usize,
    dsp_selected: Cell<ModuleKind>,
    dsp_hover: Cell<Option<dsp_panel::PanelHit>>,
    /// Processing mode to return to when A/B bypass is switched off
    pre_bypass_mode: ProcessingMode,
    module_combo_hwnd: HWND,
    hfont_ui: HFONT,
    hfont_mono: HFONT,
    hfont_bold: HFONT,
    hfont_title: HFONT,
    bg_brush: HBRUSH,
    panel_brush: HBRUSH,
    accent_brush: HBRUSH,
    meter_dim_brush: HBRUSH,
    meter_green_brush: HBRUSH,
    meter_amber_brush: HBRUSH,
    meter_red_brush: HBRUSH,
    spectrum_pen: HPEN,
    spectrum_ref_pen: HPEN,
    corr_green_brush: HBRUSH,
    corr_red_brush: HBRUSH,
    corr_dim_brush: HBRUSH,
    is_rendering: Arc<AtomicBool>,
    _midi_controller: Option<MidiController>,
    _current_filename: String,
    _reference_filename: String,
}

static mut GLOBAL_STATE: Option<AppState> = None;

pub fn run_app() -> Result<(), String> {
    unsafe {
        // Enable per-monitor High-DPI awareness v2 for crisp rendering on 4K/scaled displays
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let icce = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_BAR_CLASSES,
        };
        let _ = InitCommonControlsEx(&icce);

        // 1. Register Main Window Class
        let class_name = w!("MasteringStudioWorkspace");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: HINSTANCE(0 as _),
            hIcon: HICON(0 as _),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or(HCURSOR(0 as _)),
            hbrBackground: HBRUSH(0 as _),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class_name,
            hIconSm: HICON(0 as _),
        };

        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            return Err("Failed to register main window class".to_string());
        }

        // 2. Register Child Viewport Class (for custom DSP dashboard rendering)
        let child_class_name = w!("MasteringStudioViewport");
        let child_wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(viewport_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: HINSTANCE(0 as _),
            hIcon: HICON(0 as _),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or(HCURSOR(0 as _)),
            hbrBackground: HBRUSH(0 as _),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: child_class_name,
            hIconSm: HICON(0 as _),
        };
        let _ = RegisterClassExW(&child_wc);

        // 3. Register Meter Canvas Class (for hardware-style GDI stereo VU bars)
        let meter_class_name = w!("MasteringStudioMeterCanvas");
        let meter_wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(meter_canvas_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: HINSTANCE(0 as _),
            hIcon: HICON(0 as _),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or(HCURSOR(0 as _)),
            hbrBackground: HBRUSH(0 as _),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: meter_class_name,
            hIconSm: HICON(0 as _),
        };
        let _ = RegisterClassExW(&meter_wc);

        // 4. Register Waveform Canvas Class (for interactive mastering timeline overview)
        let waveform_class_name = w!("MasteringStudioWaveformCanvas");
        let waveform_wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(waveform_canvas_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: HINSTANCE(0 as _),
            hIcon: HICON(0 as _),
            hCursor: LoadCursorW(None, IDC_HAND).unwrap_or(HCURSOR(0 as _)),
            hbrBackground: HBRUSH(0 as _),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: waveform_class_name,
            hIconSm: HICON(0 as _),
        };
        let _ = RegisterClassExW(&waveform_wc);

        let engine = Arc::new(AudioEngine::new()?);

        // Processor list: the built-in suite first, then any VST3 plugins found in the standard
        // folders (fallback only), then an entry to browse for one
        let mut plugins = vec![(BUILTIN_SUITE_LABEL.to_string(), PathBuf::new())];
        for p in discover_vst3_plugins() {
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
            plugins.push((format!("VST3: {}", stem), p));
        }
        plugins.push((BROWSE_VST3_LABEL.to_string(), PathBuf::from(BROWSE_VST3_SENTINEL)));

        let window_w = 1280;
        let window_h = 780;

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("Mastering Studio"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            window_w,
            window_h,
            None,
            None,
            HINSTANCE(0 as _),
            None,
        ).map_err(|e| format!("CreateWindowExW failed: {:?}", e))?;

        // Enable Windows Explorer drag-and-drop audio loading
        DragAcceptFiles(hwnd, true);

        let hfont_ui = CreateFontW(
            17, 0, 0, 0, 400, 0, 0, 0, 0, 0, 0, 0, 0,
            w!("Segoe UI"),
        );

        let hfont_bold = CreateFontW(
            18, 0, 0, 0, 700, 0, 0, 0, 0, 0, 0, 0, 0,
            w!("Segoe UI"),
        );

        let hfont_title = CreateFontW(
            22, 0, 0, 0, 700, 0, 0, 0, 0, 0, 0, 0, 0,
            w!("Segoe UI"),
        );

        let hfont_mono = CreateFontW(
            15, 0, 0, 0, 600, 0, 0, 0, 0, 0, 0, 0, 0,
            w!("Consolas"),
        );

        let bg_brush = CreateSolidBrush(COLORREF(0x1B1B1E)); // Charcoal dark
        let panel_brush = CreateSolidBrush(COLORREF(0x25262B)); // Card background
        let accent_brush = CreateSolidBrush(COLORREF(0x00E5FF)); // Cyan highlight
        let meter_dim_brush = CreateSolidBrush(COLORREF(0x2B2D33));
        let meter_green_brush = CreateSolidBrush(COLORREF(0x00FF88));
        let meter_amber_brush = CreateSolidBrush(COLORREF(0x00D7FF)); // Gold/amber in BGR
        let meter_red_brush = CreateSolidBrush(COLORREF(0x3333FF)); // Red in BGR

        let spectrum_pen = CreatePen(PS_SOLID, 2, COLORREF(0x00E5FF)); // Neon cyan FFT curve
        let spectrum_ref_pen = CreatePen(PS_SOLID, 2, COLORREF(0x00D7FF)); // Gold FFT curve (auditioning ref)
        let corr_green_brush = CreateSolidBrush(COLORREF(0x00FF88));
        let corr_red_brush = CreateSolidBrush(COLORREF(0x3333FF));
        let corr_dim_brush = CreateSolidBrush(COLORREF(0x383A42));

        // Child viewport: built-in suite editor, stems/montage panels, or a fallback plugin's editor
        let child_viewport = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            child_class_name,
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN,
            20,
            106,
            1240,
            625,
            hwnd,
            HMENU(100 as _),
            HINSTANCE(0 as _),
            None,
        ).map_err(|e| format!("Child viewport CreateWindowExW failed: {:?}", e))?;

        // 1. Header Toolbar (Row 1)
        create_control(hwnd, w!("BUTTON"), w!("Open Audio File"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 20, 14, 100, 34, ID_BTN_OPEN, hfont_ui);
        create_control(hwnd, w!("BUTTON"), w!("Export Master"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 124, 14, 94, 34, ID_BTN_EXPORT, hfont_ui);
        create_control(hwnd, w!("BUTTON"), w!("Batch Folder"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 222, 14, 60, 34, ID_BTN_BATCH, hfont_ui);
        create_control(hwnd, w!("BUTTON"), w!("Save Preset"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 286, 14, 68, 34, ID_BTN_SAVE_PRESET, hfont_ui);
        create_control(hwnd, w!("BUTTON"), w!("Load Preset"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 358, 14, 68, 34, ID_BTN_LOAD_PRESET, hfont_ui);
        let target_btn = create_control(hwnd, w!("BUTTON"), w!("Target: Spotify -14"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 430, 14, 115, 34, ID_BTN_TARGET, hfont_ui);
        let montage_btn = create_control(hwnd, w!("BUTTON"), w!("Montage [M]"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 549, 14, 78, 34, ID_BTN_MONTAGE, hfont_bold);
        let stems_btn = create_control(hwnd, w!("BUTTON"), w!("Stems [S]"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 631, 14, 70, 34, ID_BTN_STEMS, hfont_bold);
        create_control(hwnd, w!("BUTTON"), w!("Device [H]"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 705, 14, 68, 34, ID_BTN_DEVICE_SETTINGS, hfont_ui);

        let combo_hwnd = create_control(hwnd, w!("COMBOBOX"), w!(""), WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(CBS_DROPDOWNLIST as u32), 777, 15, 125, 400, ID_COMBO_MODULE, hfont_ui);
        for (name, _) in &plugins {
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            let _ = SendMessageW(combo_hwnd, CB_ADDSTRING, WPARAM(0), LPARAM(wide.as_ptr() as _));
        }
        let _ = SendMessageW(combo_hwnd, CB_SETCURSEL, WPARAM(0), LPARAM(0));

        let bypass_btn = create_control(hwnd, w!("BUTTON"), w!("A/B: Mastered"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 906, 14, 82, 34, ID_BTN_BYPASS, hfont_bold);
        let mode_btn = create_control(hwnd, w!("BUTTON"), w!("Mode: VST3"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 992, 14, 68, 34, ID_BTN_MODE, hfont_ui);

        let info_label = create_control(hwnd, w!("STATIC"), w!("Ready (Space: Play | A: Assistant | R: Ref | M: Montage | S: Stems | D: Export format | H: Device)"), WS_CHILD | WS_VISIBLE, 1064, 17, 196, 30, ID_LABEL_INFO, hfont_ui);

        // 2. Transport & Scrubber Bar (Row 2)
        create_control(hwnd, w!("BUTTON"), w!("▶"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 20, 56, 48, 36, ID_BTN_PLAY, hfont_bold);
        create_control(hwnd, w!("BUTTON"), w!("⏸"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 72, 56, 48, 36, ID_BTN_PAUSE, hfont_ui);
        create_control(hwnd, w!("BUTTON"), w!("⏹"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 124, 56, 48, 36, ID_BTN_STOP, hfont_ui);
        let loop_btn = create_control(hwnd, w!("BUTTON"), w!("Loop: OFF"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 176, 56, 62, 36, ID_BTN_LOOP, hfont_ui);
        create_control(hwnd, w!("BUTTON"), w!("+ Ref Track"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 242, 56, 82, 36, ID_BTN_LOAD_REF, hfont_ui);
        let ref_toggle_btn = create_control(hwnd, w!("BUTTON"), w!("Ref: OFF [R]"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 328, 56, 88, 36, ID_BTN_TOGGLE_REF, hfont_ui);
        create_control(hwnd, w!("BUTTON"), w!("Auto-Match Gain"), WS_CHILD | WS_VISIBLE | WS_TABSTOP, 420, 56, 110, 36, ID_BTN_MATCH_GAIN, hfont_ui);

        // Hardware-Style Waveform Scrubber & Real-Time FFT Spectrum Overlay Canvas
        let waveform_canvas = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            waveform_class_name,
            w!(""),
            WS_CHILD | WS_VISIBLE,
            534,
            56,
            380,
            36,
            hwnd,
            HMENU(ID_SLIDER_TRACK as _),
            HINSTANCE(0 as _),
            None,
        ).map_err(|e| format!("Waveform canvas CreateWindowExW failed: {:?}", e))?;

        let time_label = create_control(hwnd, w!("STATIC"), w!("00:00.00 / 00:00.00"), WS_CHILD | WS_VISIBLE, 918, 63, 118, 24, ID_LABEL_TIME, hfont_mono);
        let meter_label = create_control(hwnd, w!("STATIC"), w!("I:-inf | TP:-inf"), WS_CHILD | WS_VISIBLE, 1040, 63, 135, 24, ID_LABEL_METER, hfont_mono);

        // Visual Hardware-Style VU & Phase Correlation Meter Canvas
        let meter_canvas = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            meter_class_name,
            w!(""),
            WS_CHILD | WS_VISIBLE,
            1179,
            56,
            82,
            36,
            hwnd,
            HMENU(ID_METER_CANVAS as _),
            HINSTANCE(0 as _),
            None,
        ).map_err(|e| format!("Meter canvas CreateWindowExW failed: {:?}", e))?;

        GLOBAL_STATE = Some(AppState {
            engine: Arc::clone(&engine),
            vst3_plugins: plugins,
            selected_module_idx: 0,
            child_viewport,
            waveform_canvas_hwnd: waveform_canvas,
            time_label_hwnd: time_label,
            meter_label_hwnd: meter_label,
            meter_canvas_hwnd: meter_canvas,
            info_label_hwnd: info_label,
            bypass_btn_hwnd: bypass_btn,
            loop_btn_hwnd: loop_btn,
            mode_btn_hwnd: mode_btn,
            ref_toggle_btn_hwnd: ref_toggle_btn,
            target_btn_hwnd: target_btn,
            montage_btn_hwnd: montage_btn,
            stems_btn_hwnd: stems_btn,
            in_stems_mode: false,
            current_target_idx: 0,
            export_format_idx: 0,
            dsp_selected: Cell::new(ModuleKind::Equalizer),
            dsp_hover: Cell::new(None),
            pre_bypass_mode: ProcessingMode::DspSuite,
            module_combo_hwnd: combo_hwnd,
            hfont_ui,
            hfont_mono,
            hfont_bold,
            hfont_title,
            bg_brush,
            panel_brush,
            accent_brush,
            meter_dim_brush,
            meter_green_brush,
            meter_amber_brush,
            meter_red_brush,
            spectrum_pen,
            spectrum_ref_pen,
            corr_green_brush,
            corr_red_brush,
            corr_dim_brush,
            is_rendering: Arc::new(AtomicBool::new(false)),
            _midi_controller: None,
            _current_filename: "None".to_string(),
            _reference_filename: "None".to_string(),
        });

        // Initialize WinMM MIDI hardware controller listener
        let midi_ctrl = MidiController::new(hwnd);
        if let Some(ref mut s) = GLOBAL_STATE {
            s._midi_controller = Some(midi_ctrl);
        }

        // Start on the built-in suite
        load_module_by_index(0);

        // Auto-load audio track if passed via command line (Explorer shell integration)
        if let Some(arg_path) = std::env::args().nth(1) {
            let p = PathBuf::from(&arg_path);
            if p.exists() {
                if let Ok(buf) = load_audio_file(&p) {
                    if let Some(ref mut s) = GLOBAL_STATE {
                        let filename = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                        let info_text = format!("{}: {:.1}s ({} Hz, {} frames)", filename, buf.duration_seconds(), buf.sample_rate, buf.num_frames());
                        let wide_info: Vec<u16> = info_text.encode_utf16().chain(Some(0)).collect();
                        let _ = SetWindowTextW(s.info_label_hwnd, PCWSTR(wide_info.as_ptr()));
                        s._current_filename = filename;
                        s.engine.set_audio_buffer(buf);
                        let _ = InvalidateRect(s.waveform_canvas_hwnd, None, false);
                        s.engine.play();
                    }
                }
            }
        }

        // Start 60 Hz UI timer for transport, timeline, spectrum overlay, and hardware meters
        let _ = SetTimer(hwnd, TIMER_ID_REFRESH, 16, None);

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        // Standard Windows message pump
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // Cleanup
        let _ = KillTimer(hwnd, TIMER_ID_REFRESH);
        if let Some(state) = GLOBAL_STATE.take() {
            state.engine.with_vst3_plugin(|p| {
                if let Some(plugin) = p {
                    plugin.destroy_view();
                }
            });
            let _ = state.engine.set_vst3_plugin(None);
            state.engine.collect_garbage();
            let _ = DeleteObject(HGDIOBJ(state.bg_brush.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.panel_brush.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.accent_brush.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.meter_dim_brush.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.meter_green_brush.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.meter_amber_brush.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.meter_red_brush.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.spectrum_pen.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.spectrum_ref_pen.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.corr_green_brush.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.corr_red_brush.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.corr_dim_brush.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.hfont_ui.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.hfont_mono.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.hfont_bold.0 as _));
            let _ = DeleteObject(HGDIOBJ(state.hfont_title.0 as _));
        }
    }

    Ok(())
}

fn create_control(
    parent: HWND,
    class_name: PCWSTR,
    text: PCWSTR,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    id: usize,
    font: HFONT,
) -> HWND {
    unsafe {
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            text,
            style,
            x,
            y,
            w,
            h,
            parent,
            HMENU(id as _),
            HINSTANCE(0 as _),
            None,
        ).expect("Failed to create control");

        if !font.is_invalid() {
            let _ = SendMessageW(hwnd, WM_SETFONT, WPARAM(font.0 as _), LPARAM(1));
        }
        hwnd
    }
}

const BUILTIN_SUITE_LABEL: &str = "Built-in Mastering Suite";
const BROWSE_VST3_LABEL: &str = "Load VST3 plugin (fallback)...";
const BROWSE_VST3_SENTINEL: &str = "::browse-vst3";

/// VST3 plugins in the standard per-machine folder and a `plugins` folder next to the executable.
/// Only used as optional fallback processors; the app runs fully without any.
fn discover_vst3_plugins() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from(r"C:\Program Files\Common Files\VST3")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.join("plugins"));
        }
    }
    let mut found = Vec::new();
    for root in roots {
        collect_vst3(&root, 0, &mut found);
    }
    found.sort_by_key(|p| p.file_stem().map(|s| s.to_ascii_lowercase()));
    found.dedup();
    found
}

fn collect_vst3(dir: &std::path::Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let is_vst3 = path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("vst3")).unwrap_or(false);
        if is_vst3 {
            out.push(path);
        } else if path.is_dir() && depth < 2 {
            collect_vst3(&path, depth + 1, out);
        }
    }
}

unsafe fn set_info_text(hwnd: HWND, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
}

/// Run `f` with the render source matching the current processing mode. The VST3 plugin is
/// detached from the engine for the duration, so the UI and audio thread never wait on the render.
fn with_render_source<R>(engine: &AudioEngine, mode: ProcessingMode, f: impl for<'a> FnOnce(RenderSource<'a>) -> R) -> R {
    let mut dsp_clone = engine.dsp_suite_snapshot();
    engine.with_detached_vst3_plugin(|opt_plugin| {
        let source = match mode {
            ProcessingMode::Vst3 => match opt_plugin {
                Some(plugin) => RenderSource::Vst3(plugin),
                None => RenderSource::Bypass,
            },
            ProcessingMode::DspSuite => RenderSource::Dsp(&mut dsp_clone),
            ProcessingMode::Bypass => RenderSource::Bypass,
        };
        f(source)
    })
}

/// Show the plugin editor only when it is the active view; otherwise hide it so the built-in
/// DSP, stems and montage panels are not covered by it
unsafe fn refresh_viewport_layout(state: &AppState) {
    let show_plugin = !state.in_stems_mode
        && !state.engine.in_montage_mode()
        && state.engine.processing_mode() != ProcessingMode::DspSuite;
    let view_size = state.engine.with_vst3_plugin(|p| p.map(|pl| pl.view_size()));
    let plugin_view = GetWindow(state.child_viewport, GW_CHILD).ok().filter(|h| !h.is_invalid());

    match (plugin_view, view_size) {
        (Some(view_hwnd), Some(size)) if size.width() > 0 => {
            let _ = ShowWindow(view_hwnd, if show_plugin { SW_SHOW } else { SW_HIDE });
            let (w, h) = if show_plugin { (size.width(), size.height()) } else { (VIEWPORT_W, VIEWPORT_H) };
            let _ = SetWindowPos(state.child_viewport, None, 0, 0, w, h, SWP_NOZORDER | SWP_NOMOVE);
        }
        _ => {
            let _ = SetWindowPos(state.child_viewport, None, 0, 0, VIEWPORT_W, VIEWPORT_H, SWP_NOZORDER | SWP_NOMOVE);
        }
    }
    let _ = InvalidateRect(state.child_viewport, None, true);
}

/// Rebuild the montage playback buffer on a worker thread (decoding every track can take a while)
fn spawn_montage_refresh(state: &AppState) {
    let engine = Arc::clone(&state.engine);
    let info_hwnd_raw = state.info_label_hwnd.0 as usize;
    let wave_hwnd_raw = state.waveform_canvas_hwnd.0 as usize;
    std::thread::spawn(move || unsafe {
        let info_hwnd = HWND(info_hwnd_raw as *mut _);
        set_info_text(info_hwnd, "Montage: sequencing album for playback...");
        match engine.refresh_montage_playback() {
            Ok(()) => {
                let msg = format!("Montage ready for playback: {:.1}s", engine.duration_seconds());
                set_info_text(info_hwnd, &msg);
                let _ = InvalidateRect(HWND(wave_hwnd_raw as *mut _), None, true);
            }
            Err(e) => set_info_text(info_hwnd, &format!("Montage playback: {}", e)),
        }
    });
}

/// Decode and append audio files to the album montage, then rebuild montage playback
unsafe fn add_montage_files(state: &AppState, files: Vec<PathBuf>) {
    let mut added = 0;
    for p in files {
        if let Ok(buf) = load_audio_file(&p) {
            let dur = buf.duration_seconds();
            let sr = buf.sample_rate;
            let lufs = calculate_buffer_integrated_lufs(&buf);
            let tp = buf.peaks().iter().fold(0.0001f32, |acc, p| acc.max(p.max_l).max(p.max_r));
            let tp_db = (20.0f32 * tp.log10()).clamp(-60.0f32, 6.0f32);
            state.engine.with_montage(|m| {
                m.add_track(p.clone(), dur, sr);
                if let Some(last) = m.tracks.last_mut() {
                    last.integrated_lufs = lufs;
                    last.true_peak_dbtp = tp_db;
                }
            });
            added += 1;
        }
    }
    let _ = InvalidateRect(state.child_viewport, None, true);
    if added > 0 {
        spawn_montage_refresh(state);
    }
}

unsafe fn load_module_by_index(index: usize) {
    let state = match GLOBAL_STATE.as_mut() {
        Some(s) => s,
        None => return,
    };

    if index >= state.vst3_plugins.len() {
        return;
    }

    if state.vst3_plugins[index].1.as_os_str() == BROWSE_VST3_SENTINEL {
        // Browse for a plugin, add it to the list just above this entry, and load it
        let picked = rfd::FileDialog::new()
            .set_title("Load a VST3 plugin as a fallback processor")
            .add_filter("VST3 plugin", &["vst3"])
            .pick_file();
        let Some(path) = picked else {
            let _ = SendMessageW(state.module_combo_hwnd, CB_SETCURSEL, WPARAM(state.selected_module_idx), LPARAM(0));
            return;
        };
        let label = format!("VST3: {}", path.file_stem().unwrap_or_default().to_string_lossy());
        let wide: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
        let _ = SendMessageW(state.module_combo_hwnd, CB_INSERTSTRING, WPARAM(index), LPARAM(wide.as_ptr() as _));
        state.vst3_plugins.insert(index, (label, path));
        let _ = SendMessageW(state.module_combo_hwnd, CB_SETCURSEL, WPARAM(index), LPARAM(0));
    }

    state.selected_module_idx = index;
    let (ref name, ref path) = state.vst3_plugins[index];

    // Detach the old plugin's editor and take it off the audio thread
    state.engine.with_vst3_plugin(|p| {
        if let Some(old_plugin) = p {
            old_plugin.destroy_view();
        }
    });
    let _ = state.engine.set_vst3_plugin(None);

    // Built-in suite chosen
    if path.as_os_str().is_empty() {
        println!("Using the built-in mastering suite");
        state.engine.set_processing_mode(ProcessingMode::DspSuite);
        state.pre_bypass_mode = ProcessingMode::DspSuite;
        let _ = SetWindowTextW(state.mode_btn_hwnd, w!("Mode: Built-in"));
        refresh_viewport_layout(state);
        return;
    }

    println!("Loading fallback VST3 plugin: {} ({})", name, path.display());
    match Vst3Plugin::load(path) {
        Ok(plugin) => {
            match plugin.create_view(state.child_viewport) {
                Ok(view_rect) => {
                    println!("Attached plugin editor: {}x{}", view_rect.width(), view_rect.height());
                    let req_w = (view_rect.width() + 50).max(1200);
                    let req_h = (view_rect.height() + 155).max(750);
                    let parent = GetParent(state.child_viewport).unwrap_or_default();
                    let _ = SetWindowPos(
                        parent,
                        None,
                        0,
                        0,
                        req_w,
                        req_h,
                        SWP_NOZORDER | SWP_NOMOVE,
                    );
                    let _ = SetWindowPos(
                        state.child_viewport,
                        None,
                        25,
                        120,
                        view_rect.width(),
                        view_rect.height(),
                        SWP_NOZORDER | SWP_NOMOVE,
                    );
                }
                Err(e) => {
                    eprintln!("Failed to attach plugin view: {}", e);
                }
            }

            if let Err(e) = state.engine.set_vst3_plugin(Some(plugin)) {
                eprintln!("Failed to configure VST3 module {}: {}", path.display(), e);
                set_info_text(state.info_label_hwnd, &format!("Plugin setup failed: {}", e));
                return;
            }
            state.engine.set_processing_mode(ProcessingMode::Vst3);
            state.pre_bypass_mode = ProcessingMode::Vst3;
            let _ = SetWindowTextW(state.mode_btn_hwnd, w!("Mode: VST3"));
            refresh_viewport_layout(state);
            // Redraw surrounding area to clear any previous taller plugin artifacts
            let _ = InvalidateRect(GetParent(state.child_viewport).unwrap_or_default(), None, true);
        }
        Err(e) => {
            eprintln!("Failed to load VST3 module {}: {}", path.display(), e);
        }
    }
}

unsafe fn draw_text_str(hdc: HDC, text: &str, rect: &mut RECT, format: DRAW_TEXT_FORMAT) {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let _ = DrawTextW(hdc, &mut wide, rect, format);
}

/// Custom window procedure for the central viewport (renders mastering dashboard when in DSP mode)
unsafe extern "system" fn viewport_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            if let Some(ref state) = GLOBAL_STATE {
                if state.in_stems_mode {
                    let x = (lparam.0 & 0xFFFF) as i16 as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

                    // Button 1: [⚡ Auto-Separate Master (6 Stems)] (20..310, 75..105)
                    if x >= 20 && x <= 310 && y >= 75 && y <= 105 {
                        let engine_clone = Arc::clone(&state.engine);
                        let info_hwnd_raw = state.info_label_hwnd.0 as usize;
                        let child_hwnd_raw = hwnd.0 as usize;
                        let wave_hwnd_raw = state.waveform_canvas_hwnd.0 as usize;
                        let is_rendering_flag = Arc::clone(&state.is_rendering);
                        is_rendering_flag.store(true, Ordering::Relaxed);

                        std::thread::spawn(move || {
                            let res = engine_clone.separate_active_track_into_stems(move |progress, step_msg| {
                                let pct = (progress * 100.0) as i32;
                                let text = format!("Separating Stems [{}%]: {}", pct, step_msg);
                                let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
                                unsafe {
                                    let info_hwnd = HWND(info_hwnd_raw as *mut _);
                                    let _ = SetWindowTextW(info_hwnd, PCWSTR(wide.as_ptr()));
                                }
                            });

                            let child_hwnd = HWND(child_hwnd_raw as *mut _);
                            let wave_hwnd = HWND(wave_hwnd_raw as *mut _);
                            let info_hwnd = HWND(info_hwnd_raw as *mut _);

                            is_rendering_flag.store(false, Ordering::Relaxed);
                            match res {
                                Ok(_) => {
                                    let msg = "Stem separation complete! 6 high-fidelity stems ready for mixing/mastering.";
                                    let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                                    unsafe {
                                        let _ = SetWindowTextW(info_hwnd, PCWSTR(wide.as_ptr()));
                                        let _ = InvalidateRect(child_hwnd, None, true);
                                        let _ = InvalidateRect(wave_hwnd, None, true);
                                    }
                                }
                                Err(e) => {
                                    let msg = format!("Stem separation failed: {}", e);
                                    let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                                    unsafe {
                                        let _ = SetWindowTextW(info_hwnd, PCWSTR(wide.as_ptr()));
                                    }
                                }
                            }
                        });
                        return LRESULT(0);
                    }

                    // Button 2: [⚡ Sum Stems to Master] (320..510, 75..105)
                    if x >= 320 && x <= 510 && y >= 75 && y <= 105 {
                        let res = state.engine.update_master_from_stems();
                        match res {
                            Ok(_) => {
                                let msg = "Stems summed into master audio buffer";
                                let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                                unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                                let _ = InvalidateRect(state.waveform_canvas_hwnd, None, true);
                            }
                            Err(e) => {
                                let msg = format!("Sum stems failed: {}", e);
                                let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                                unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                            }
                        }
                        let _ = InvalidateRect(hwnd, None, true);
                        return LRESULT(0);
                    }

                    // Button 3: [💾 Export All 6 Stems to Folder] (520..770, 75..105)
                    if x >= 520 && x <= 770 && y >= 75 && y <= 105 {
                        if let Some(folder) = rfd::FileDialog::new()
                            .set_title("Select Folder to Export 6 Stems (24-bit WAV)")
                            .pick_folder()
                        {
                            let base_name = if !state._current_filename.is_empty() {
                                state._current_filename.clone()
                            } else {
                                "Track".to_string()
                            };
                            let export_res = state.engine.with_stem_mixer(|sm| sm.export_all_stems(&folder, &base_name));
                            match export_res {
                                Ok(files) => {
                                    let msg = format!("Successfully exported {} stems to {}", files.len(), folder.display());
                                    let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                                    unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                                }
                                Err(err) => {
                                    let msg = format!("Export stems failed: {}", err);
                                    let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                                    unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                                }
                            }
                        }
                        return LRESULT(0);
                    }

                    // Button 4: [🗑 Clear Stems] (780..910, 75..105)
                    if x >= 780 && x <= 910 && y >= 75 && y <= 105 {
                        state.engine.with_stem_mixer(|sm| sm.clear_all());
                        let _ = InvalidateRect(hwnd, None, true);
                        return LRESULT(0);
                    }

                    // Check clicks on each of the 6 stem channel strips
                    for i in 0..6 {
                        let sx = 20 + i as i32 * 202;
                        if x >= sx && x <= sx + 190 {
                            // 1. [📂 Load Stem Audio...] button (y: 192..224)
                            if y >= 192 && y <= 224 {
                                if let Some(path) = rfd::FileDialog::new()
                                    .set_title(&format!("Select Audio File for Stem {}", i + 1))
                                    .add_filter("Audio Files", &["wav", "mp3", "flac", "aif", "aiff", "aac", "m4a"])
                                    .pick_file()
                                {
                                    let load_res = state.engine.with_stem_mixer(|sm| sm.stems[i].load_file(path));
                                    if load_res.is_ok() {
                                        let _ = state.engine.update_master_from_stems();
                                        let _ = InvalidateRect(state.waveform_canvas_hwnd, None, true);
                                    }
                                    let _ = InvalidateRect(hwnd, None, true);
                                }
                                return LRESULT(0);
                            }

                            // 2. Gain [-] button (x: sx + 12..sx + 50, y: 268..300)
                            if x >= sx + 12 && x <= sx + 50 && y >= 268 && y <= 300 {
                                state.engine.with_stem_mixer(|sm| {
                                    let cur = sm.stems[i].gain_db;
                                    sm.set_stem_gain(i, cur - 1.0);
                                });
                                let _ = state.engine.update_master_from_stems();
                                let _ = InvalidateRect(hwnd, None, true);
                                return LRESULT(0);
                            }

                            // 3. Gain [+] button (x: sx + 140..sx + 178, y: 268..300)
                            if x >= sx + 140 && x <= sx + 178 && y >= 268 && y <= 300 {
                                state.engine.with_stem_mixer(|sm| {
                                    let cur = sm.stems[i].gain_db;
                                    sm.set_stem_gain(i, cur + 1.0);
                                });
                                let _ = state.engine.update_master_from_stems();
                                let _ = InvalidateRect(hwnd, None, true);
                                return LRESULT(0);
                            }

                            // 4. Pan [<] button (x: sx + 12..sx + 50, y: 350..382)
                            if x >= sx + 12 && x <= sx + 50 && y >= 350 && y <= 382 {
                                state.engine.with_stem_mixer(|sm| {
                                    let cur = sm.stems[i].pan;
                                    sm.set_stem_pan(i, cur - 0.1);
                                });
                                let _ = state.engine.update_master_from_stems();
                                let _ = InvalidateRect(hwnd, None, true);
                                return LRESULT(0);
                            }

                            // 5. Pan [>] button (x: sx + 140..sx + 178, y: 350..382)
                            if x >= sx + 140 && x <= sx + 178 && y >= 350 && y <= 382 {
                                state.engine.with_stem_mixer(|sm| {
                                    let cur = sm.stems[i].pan;
                                    sm.set_stem_pan(i, cur + 0.1);
                                });
                                let _ = state.engine.update_master_from_stems();
                                let _ = InvalidateRect(hwnd, None, true);
                                return LRESULT(0);
                            }

                            // 6. [M] MUTE button (x: sx + 12..sx + 90, y: 410..448)
                            if x >= sx + 12 && x <= sx + 90 && y >= 410 && y <= 448 {
                                state.engine.with_stem_mixer(|sm| sm.toggle_stem_mute(i));
                                let _ = state.engine.update_master_from_stems();
                                let _ = InvalidateRect(hwnd, None, true);
                                return LRESULT(0);
                            }

                            // 7. [S] SOLO button (x: sx + 98..sx + 178, y: 410..448)
                            if x >= sx + 98 && x <= sx + 178 && y >= 410 && y <= 448 {
                                state.engine.with_stem_mixer(|sm| sm.toggle_stem_solo(i));
                                let _ = state.engine.update_master_from_stems();
                                let _ = InvalidateRect(hwnd, None, true);
                                return LRESULT(0);
                            }
                        }
                    }

                    return LRESULT(0);
                } else if state.engine.in_montage_mode() {
                    let x = (lparam.0 & 0xFFFF) as i16 as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

                    // Button 1: [+ Add Track] (20..160, 75..105)
                    if x >= 20 && x <= 160 && y >= 75 && y <= 105 {
                        if let Some(files) = rfd::FileDialog::new()
                            .set_title("Add Audio Tracks to Album Montage")
                            .add_filter("Audio Files", &["wav", "mp3", "flac", "aif", "aiff", "aac", "m4a"])
                            .pick_files()
                        {
                            add_montage_files(state, files);
                        }
                        return LRESULT(0);
                    }

                    // Button 2: [⚡ Auto-Balance LUFS] (170..340, 75..105)
                    if x >= 170 && x <= 340 && y >= 75 && y <= 105 {
                        state.engine.with_montage(|m| m.auto_balance_album_lufs());
                        set_info_text(state.info_label_hwnd, "Auto-balanced album tracks to unified target loudness");
                        let _ = InvalidateRect(hwnd, None, true);
                        spawn_montage_refresh(state);
                        return LRESULT(0);
                    }

                    // Button 3: [💾 Export Album CUE+WAV] (350..540, 75..105)
                    if x >= 350 && x <= 540 && y >= 75 && y <= 105 {
                        if let Some(save_wav) = rfd::FileDialog::new()
                            .set_title("Export Mastered Album Audio & Red Book CUE Sheet")
                            .set_file_name("Album_Master.wav")
                            .add_filter("Waveform Audio", &["wav"])
                            .save_file()
                        {
                            if state.is_rendering.swap(true, Ordering::Relaxed) {
                                set_info_text(state.info_label_hwnd, "A render is already running");
                                return LRESULT(0);
                            }
                            state.engine.pause();
                            let info_hwnd_raw = state.info_label_hwnd.0 as usize;
                            let montage_clone = state.engine.with_montage(|m| m.clone());
                            let is_rendering_flag = Arc::clone(&state.is_rendering);
                            let engine = Arc::clone(&state.engine);
                            let mode = state.engine.processing_mode();
                            let format = EXPORT_FORMATS[state.export_format_idx];

                            std::thread::spawn(move || {
                                let info_hwnd = HWND(info_hwnd_raw as *mut _);
                                let res = with_render_source(&engine, mode, |source| {
                                    montage_clone.export_album_with_source(
                                        &save_wav,
                                        format.bit_depth,
                                        format.sample_rate,
                                        source,
                                        |progress, step_msg| {
                                            let text = format!("Album [{}%]: {}", (progress * 100.0) as i32, step_msg);
                                            unsafe { set_info_text(info_hwnd, &text); }
                                        },
                                    )
                                });
                                is_rendering_flag.store(false, Ordering::Relaxed);
                                match res {
                                    Ok(cue_path) => {
                                        let done = format!("Album Export Complete: CUE + WAV ready in {}", cue_path.parent().unwrap_or(std::path::Path::new("")).display());
                                        let wide: Vec<u16> = done.encode_utf16().chain(Some(0)).collect();
                                        unsafe { let _ = SetWindowTextW(info_hwnd, PCWSTR(wide.as_ptr())); }
                                    }
                                    Err(err) => {
                                        let err_msg = format!("Album Export Failed: {}", err);
                                        let wide: Vec<u16> = err_msg.encode_utf16().chain(Some(0)).collect();
                                        unsafe { let _ = SetWindowTextW(info_hwnd, PCWSTR(wide.as_ptr())); }
                                    }
                                }
                            });
                        }
                        return LRESULT(0);
                    }

                    // Button 4: [🗑 Clear] (550..650, 75..105)
                    if x >= 550 && x <= 650 && y >= 75 && y <= 105 {
                        state.engine.with_montage(|m| m.tracks.clear());
                        let _ = InvalidateRect(hwnd, None, true);
                        spawn_montage_refresh(state);
                        return LRESULT(0);
                    }

                    // Track rows (y: 150 + 32 * idx): '#' moves up, Gap cycles, Status toggles
                    if let Some(idx) = montage_row_at(y) {
                        let changed = state.engine.with_montage(|m| {
                            if idx >= m.tracks.len() {
                                return false;
                            }
                            if x >= 20 && x < 60 {
                                if idx > 0 {
                                    m.move_track(idx, idx - 1);
                                }
                            } else if x >= 870 && x < 975 {
                                let gap = m.tracks[idx].pause_gap_seconds;
                                m.tracks[idx].pause_gap_seconds = if gap >= 3.0 { 0.0 } else { gap.floor() + 1.0 };
                            } else if x >= 980 && x < 1100 {
                                m.tracks[idx].is_active = !m.tracks[idx].is_active;
                            } else {
                                return false;
                            }
                            true
                        });
                        if changed {
                            let _ = InvalidateRect(hwnd, None, true);
                            spawn_montage_refresh(state);
                        }
                    }
                    return LRESULT(0);
                }

                // Built-in suite editor
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let fine = (GetKeyState(0x10) as u16 & 0x8000) != 0; // VK_SHIFT
                dsp_panel_gesture(state, hwnd, x, y, 1, fine, false);
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            if let Some(ref state) = GLOBAL_STATE {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                if state.in_stems_mode {
                    return LRESULT(0);
                }
                if state.engine.in_montage_mode() {
                    // Right-click a track row to remove it from the album
                    if let Some(idx) = montage_row_at(y) {
                        let removed = state.engine.with_montage(|m| match m.tracks.get(idx).map(|t| t.id) {
                            Some(id) => {
                                m.remove_track(id);
                                true
                            }
                            None => false,
                        });
                        if removed {
                            let _ = InvalidateRect(hwnd, None, true);
                            spawn_montage_refresh(state);
                        }
                    }
                    return LRESULT(0);
                }
                let fine = (GetKeyState(0x10) as u16 & 0x8000) != 0;
                dsp_panel_gesture(state, hwnd, x, y, -1, fine, false);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(ref state) = GLOBAL_STATE {
                if !state.in_stems_mode && !state.engine.in_montage_mode() {
                    let x = (lparam.0 & 0xFFFF) as i16 as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    let selected = state.dsp_selected.get();
                    let hover = state.engine.with_dsp_suite(|s| dsp_panel::hit_test(s, selected, x, y));
                    if hover != state.dsp_hover.get() {
                        state.dsp_hover.set(hover);
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);

            if let Some(ref state) = GLOBAL_STATE {
                let mut client_rect: RECT = std::mem::zeroed();
                let _ = GetClientRect(hwnd, &mut client_rect);

                // Fill background
                let _ = FillRect(hdc, &client_rect, state.bg_brush);

                if state.in_stems_mode {
                    // Header
                    let _ = SetBkMode(hdc, TRANSPARENT);
                    let _ = SetTextColor(hdc, COLORREF(0x00E5FF)); // Neon Cyan
                    let _ = SelectObject(hdc, HGDIOBJ(state.hfont_title.0 as _));
                    let mut title_rect = RECT { left: 20, top: 15, right: 1100, bottom: 45 };
                    draw_text_str(hdc, "  6-Channel Stem Mastering & AI Separation Workstation", &mut title_rect, DT_LEFT | DT_SINGLELINE | DT_VCENTER);

                    // Subtitle
                    let _ = SetTextColor(hdc, COLORREF(0xAAAAAA));
                    let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));
                    let mut sub_rect = RECT { left: 20, top: 45, right: 1100, bottom: 70 };
                    draw_text_str(hdc, "  STFT Spectral Masking • Harmonic-Percussive (HPSS) Demixing • Azimuth Stereophonic Separation • Bit-Transparent Summation", &mut sub_rect, DT_LEFT | DT_SINGLELINE | DT_VCENTER);

                    // Action Buttons (y: 75..105)
                    let btn_specs = [
                        (20, 310, "⚡ Auto-Separate Master (6 Stems)", state.panel_brush, COLORREF(0x00E5FF)),
                        (320, 510, "⚡ Sum Stems to Master", state.panel_brush, COLORREF(0x00FF88)),
                        (520, 770, "💾 Export All 6 Stems to Folder", state.panel_brush, COLORREF(0x00D7FF)),
                        (780, 910, "🗑 Clear Stems", state.panel_brush, COLORREF(0x6666FF)),
                    ];

                    for &(bx1, bx2, btext, bbrush, text_color) in &btn_specs {
                        let btn_rect = RECT { left: bx1, top: 75, right: bx2, bottom: 105 };
                        let _ = FillRect(hdc, &btn_rect, bbrush);
                        let _ = FrameRect(hdc, &btn_rect, state.meter_dim_brush);
                        let _ = SetTextColor(hdc, text_color);
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
                        let mut tr = btn_rect;
                        draw_text_str(hdc, btext, &mut tr, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
                    }

                    let stems_data = state.engine.with_stem_mixer(|sm| sm.stems.clone());

                    for (i, stem) in stems_data.iter().enumerate() {
                        let sx = 20 + i as i32 * 202;
                        let card_rect = RECT { left: sx, top: 120, right: sx + 190, bottom: 580 };
                        let _ = FillRect(hdc, &card_rect, state.panel_brush);
                        let _ = FrameRect(hdc, &card_rect, state.meter_dim_brush);

                        // Card Header
                        let hdr_rect = RECT { left: sx, top: 120, right: sx + 190, bottom: 152 };
                        let _ = FillRect(hdc, &hdr_rect, state.meter_dim_brush);
                        let _ = SetTextColor(hdc, COLORREF(stem.stem_type.color_bgr()));
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
                        let mut htr = RECT { left: sx + 10, top: 120, right: sx + 180, bottom: 152 };
                        draw_text_str(hdc, stem.stem_type.default_label(), &mut htr, DT_LEFT | DT_SINGLELINE | DT_VCENTER);

                        // File Name or Empty Status
                        let file_label = if let Some(ref p) = stem.file_path {
                            p.file_name().unwrap_or_default().to_string_lossy().to_string()
                        } else if stem.buffer.is_some() {
                            stem.name.clone()
                        } else {
                            "(No audio loaded)".to_string()
                        };
                        let _ = SetTextColor(hdc, if stem.buffer.is_some() { COLORREF(0xFFFFFF) } else { COLORREF(0x777777) });
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));
                        let mut fl_rect = RECT { left: sx + 10, top: 156, right: sx + 180, bottom: 186 };
                        draw_text_str(hdc, &file_label, &mut fl_rect, DT_LEFT | DT_SINGLELINE | DT_VCENTER);

                        // [📂 Load...] Button (y: 192..224)
                        let load_rect = RECT { left: sx + 12, top: 192, right: sx + 178, bottom: 224 };
                        let _ = FillRect(hdc, &load_rect, state.bg_brush);
                        let _ = FrameRect(hdc, &load_rect, state.meter_dim_brush);
                        let _ = SetTextColor(hdc, COLORREF(0x00E5FF));
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
                        let mut ltr = load_rect;
                        draw_text_str(hdc, "📂 Load Stem...", &mut ltr, DT_CENTER | DT_SINGLELINE | DT_VCENTER);

                        // Gain Section (y: 236..310)
                        let _ = SetTextColor(hdc, COLORREF(0xAAAAAA));
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));
                        let mut g_title = RECT { left: sx + 12, top: 236, right: sx + 178, bottom: 258 };
                        draw_text_str(hdc, "Gain Level", &mut g_title, DT_LEFT | DT_SINGLELINE | DT_VCENTER);

                        // Gain Readout Box
                        let g_box = RECT { left: sx + 55, top: 268, right: sx + 135, bottom: 300 };
                        let _ = FillRect(hdc, &g_box, state.bg_brush);
                        let _ = FrameRect(hdc, &g_box, state.meter_dim_brush);
                        let gain_str = format!("{:+0.1} dB", stem.gain_db);
                        let _ = SetTextColor(hdc, COLORREF(0xFFFFFF));
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_mono.0 as _));
                        let mut gtr = g_box;
                        draw_text_str(hdc, &gain_str, &mut gtr, DT_CENTER | DT_SINGLELINE | DT_VCENTER);

                        // Gain [-] and [+] Buttons
                        let g_minus = RECT { left: sx + 12, top: 268, right: sx + 50, bottom: 300 };
                        let _ = FillRect(hdc, &g_minus, state.bg_brush);
                        let _ = FrameRect(hdc, &g_minus, state.meter_dim_brush);
                        let _ = SetTextColor(hdc, COLORREF(0x00E5FF));
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
                        let mut mtr = g_minus;
                        draw_text_str(hdc, "-", &mut mtr, DT_CENTER | DT_SINGLELINE | DT_VCENTER);

                        let g_plus = RECT { left: sx + 140, top: 268, right: sx + 178, bottom: 300 };
                        let _ = FillRect(hdc, &g_plus, state.bg_brush);
                        let _ = FrameRect(hdc, &g_plus, state.meter_dim_brush);
                        let _ = SetTextColor(hdc, COLORREF(0x00E5FF));
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
                        let mut ptr = g_plus;
                        draw_text_str(hdc, "+", &mut ptr, DT_CENTER | DT_SINGLELINE | DT_VCENTER);

                        // Pan Section (y: 318..392)
                        let _ = SetTextColor(hdc, COLORREF(0xAAAAAA));
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));
                        let mut p_title = RECT { left: sx + 12, top: 318, right: sx + 178, bottom: 340 };
                        draw_text_str(hdc, "Stereo Pan", &mut p_title, DT_LEFT | DT_SINGLELINE | DT_VCENTER);

                        let p_box = RECT { left: sx + 55, top: 350, right: sx + 135, bottom: 382 };
                        let _ = FillRect(hdc, &p_box, state.bg_brush);
                        let _ = FrameRect(hdc, &p_box, state.meter_dim_brush);
                        let pan_str = if stem.pan.abs() < 0.05 {
                            "Center".to_string()
                        } else if stem.pan < 0.0 {
                            format!("L {:0.0}%", stem.pan.abs() * 100.0)
                        } else {
                            format!("R {:0.0}%", stem.pan * 100.0)
                        };
                        let _ = SetTextColor(hdc, COLORREF(0xFFFFFF));
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_mono.0 as _));
                        let mut ptr2 = p_box;
                        draw_text_str(hdc, &pan_str, &mut ptr2, DT_CENTER | DT_SINGLELINE | DT_VCENTER);

                        let p_left = RECT { left: sx + 12, top: 350, right: sx + 50, bottom: 382 };
                        let _ = FillRect(hdc, &p_left, state.bg_brush);
                        let _ = FrameRect(hdc, &p_left, state.meter_dim_brush);
                        let _ = SetTextColor(hdc, COLORREF(0x00E5FF));
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
                        let mut ltr2 = p_left;
                        draw_text_str(hdc, "<", &mut ltr2, DT_CENTER | DT_SINGLELINE | DT_VCENTER);

                        let p_right = RECT { left: sx + 140, top: 350, right: sx + 178, bottom: 382 };
                        let _ = FillRect(hdc, &p_right, state.bg_brush);
                        let _ = FrameRect(hdc, &p_right, state.meter_dim_brush);
                        let _ = SetTextColor(hdc, COLORREF(0x00E5FF));
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
                        let mut rtr2 = p_right;
                        draw_text_str(hdc, ">", &mut rtr2, DT_CENTER | DT_SINGLELINE | DT_VCENTER);

                        // MUTE / SOLO Buttons (y: 410..448)
                        let mute_rect = RECT { left: sx + 12, top: 410, right: sx + 90, bottom: 448 };
                        let mute_brush = if stem.mute { state.meter_amber_brush } else { state.bg_brush };
                        let mute_color = if stem.mute { COLORREF(0x000000) } else { COLORREF(0x888888) };
                        let _ = FillRect(hdc, &mute_rect, mute_brush);
                        let _ = FrameRect(hdc, &mute_rect, state.meter_dim_brush);
                        let _ = SetTextColor(hdc, mute_color);
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
                        let mut mtr3 = mute_rect;
                        draw_text_str(hdc, "MUTE", &mut mtr3, DT_CENTER | DT_SINGLELINE | DT_VCENTER);

                        let solo_rect = RECT { left: sx + 98, top: 410, right: sx + 178, bottom: 448 };
                        let solo_brush = if stem.solo { state.meter_green_brush } else { state.bg_brush };
                        let solo_color = if stem.solo { COLORREF(0x000000) } else { COLORREF(0x888888) };
                        let _ = FillRect(hdc, &solo_rect, solo_brush);
                        let _ = FrameRect(hdc, &solo_rect, state.meter_dim_brush);
                        let _ = SetTextColor(hdc, solo_color);
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
                        let mut str3 = solo_rect;
                        draw_text_str(hdc, "SOLO", &mut str3, DT_CENTER | DT_SINGLELINE | DT_VCENTER);

                        // Audio specs readout at bottom of card (y: 470..500)
                        if let Some(ref buf) = stem.buffer {
                            let spec_str = format!("{} Hz • {:.1}s", buf.sample_rate, buf.duration_seconds());
                            let _ = SetTextColor(hdc, COLORREF(0x00FF88));
                            let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));
                            let mut st_rect = RECT { left: sx + 10, top: 470, right: sx + 180, bottom: 495 };
                            draw_text_str(hdc, &spec_str, &mut st_rect, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
                        }
                    }

                    let _ = EndPaint(hwnd, &ps);
                    return LRESULT(0);
                } else if state.engine.in_montage_mode() {
                    // Header
                    let _ = SetBkMode(hdc, TRANSPARENT);
                    let _ = SetTextColor(hdc, COLORREF(0x00E5FF)); // Neon Cyan
                    let _ = SelectObject(hdc, HGDIOBJ(state.hfont_title.0 as _));
                    let mut title_rect = RECT { left: 20, top: 15, right: 1100, bottom: 45 };
                    draw_text_str(hdc, "  Album Mastering Montage & Red Book CD Authoring", &mut title_rect, DT_LEFT | DT_SINGLELINE | DT_VCENTER);

                    // Subtitle
                    let _ = SetTextColor(hdc, COLORREF(0xAAAAAA));
                    let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));
                    let mut sub_rect = RECT { left: 20, top: 45, right: 1100, bottom: 70 };
                    draw_text_str(hdc, "  EBU R128 Per-Track Loudness Matching • Non-Destructive CD Pregaps • Continuous DDP / CUE Sheet Generation", &mut sub_rect, DT_LEFT | DT_SINGLELINE | DT_VCENTER);

                    // Action Buttons (y: 75..105)
                    let btn_specs = [
                        (20, 160, "+ Add Track", state.accent_brush, COLORREF(0x00E5FF)),
                        (170, 340, "⚡ Auto-Balance LUFS", state.panel_brush, COLORREF(0x00FF88)),
                        (350, 540, "💾 Export Album CUE+WAV", state.panel_brush, COLORREF(0xFFCC00)),
                        (550, 650, "🗑 Clear", state.panel_brush, COLORREF(0xFF6666)),
                    ];

                    for &(bx1, bx2, btext, bbrush, text_color) in &btn_specs {
                        let btn_rect = RECT { left: bx1, top: 75, right: bx2, bottom: 105 };
                        let _ = FillRect(hdc, &btn_rect, bbrush);
                        let _ = FrameRect(hdc, &btn_rect, state.meter_dim_brush);
                        let _ = SetTextColor(hdc, text_color);
                        let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
                        let mut tr = btn_rect;
                        draw_text_str(hdc, btext, &mut tr, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
                    }

                    // Table Header (y: 115..145)
                    let hdr_rect = RECT { left: 20, top: 115, right: 1100, bottom: 145 };
                    let _ = FillRect(hdc, &hdr_rect, state.panel_brush);
                    let _ = FrameRect(hdc, &hdr_rect, state.meter_dim_brush);

                    let cols = [
                        ("#", 25, 60),
                        ("Track Title", 65, 420),
                        ("Duration", 425, 520),
                        ("Integrated LUFS", 525, 645),
                        ("True Peak", 650, 755),
                        ("Gain Offset", 760, 865),
                        ("Pause Gap", 870, 975),
                        ("Status", 980, 1095),
                    ];

                    let _ = SetTextColor(hdc, COLORREF(0x00E5FF));
                    let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
                    for &(chdr, cx1, cx2) in &cols {
                        let mut cr = RECT { left: cx1, top: 115, right: cx2, bottom: 145 };
                        draw_text_str(hdc, chdr, &mut cr, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
                    }

                    // Render Track Rows
                    let tracks = state.engine.with_montage(|m| m.tracks.clone());
                    let mut row_y = 150;
                    let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));

                    if tracks.is_empty() {
                        let _ = SetTextColor(hdc, COLORREF(0x777777));
                        let mut empty_rect = RECT { left: 20, top: 160, right: 1100, bottom: 220 };
                        draw_text_str(hdc, "Album Montage is empty. Click '+ Add Track' or drag audio files here to assemble your album.", &mut empty_rect, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
                    } else {
                        for (idx, track) in tracks.iter().take(10).enumerate() {
                            let row_rect = RECT { left: 20, top: row_y, right: 1100, bottom: row_y + 30 };
                            let row_brush = if idx % 2 == 0 { state.panel_brush } else { state.bg_brush };
                            let _ = FillRect(hdc, &row_rect, row_brush);
                            let _ = FrameRect(hdc, &row_rect, state.corr_dim_brush);

                            let dur_m = (track.duration_seconds / 60.0).floor() as i32;
                            let dur_s = (track.duration_seconds % 60.0).floor() as i32;
                            let dur_str = format!("{:02}:{:02}", dur_m, dur_s);
                            let lufs_str = format!("{:.1} LUFS", track.integrated_lufs);
                            let tp_str = format!("{:.1} dBTP", track.true_peak_dbtp);
                            let gain_str = format!("{:+.1} dB", track.gain_offset_db);
                            let gap_str = format!("{:.1} s", track.pause_gap_seconds);

                            let values = [
                                (format!("{:02}", idx + 1), 25, 60, COLORREF(0xAAAAAA)),
                                (track.title.clone(), 65, 420, COLORREF(0xFFFFFF)),
                                (dur_str, 425, 520, COLORREF(0xDDDDDD)),
                                (lufs_str, 525, 645, if track.integrated_lufs > -14.5 && track.integrated_lufs < -13.5 { COLORREF(0x00FF88) } else { COLORREF(0xFFCC00) }),
                                (tp_str, 650, 755, if track.true_peak_dbtp > -0.5 { COLORREF(0xFF5555) } else { COLORREF(0x00FF88) }),
                                (gain_str, 760, 865, COLORREF(0x00E5FF)),
                                (gap_str, 870, 975, COLORREF(0xAAAAAA)),
                                if track.is_active {
                                    ("[READY]".to_string(), 980, 1095, COLORREF(0x00FF88))
                                } else {
                                    ("[SKIPPED]".to_string(), 980, 1095, COLORREF(0x777777))
                                },
                            ];

                            for (val, x1, x2, color) in values {
                                let _ = SetTextColor(hdc, color);
                                let mut cell_rect = RECT { left: x1, top: row_y, right: x2, bottom: row_y + 30 };
                                draw_text_str(hdc, &val, &mut cell_rect, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
                            }

                            row_y += 32;
                        }
                    }

                    let _ = SetTextColor(hdc, COLORREF(0x777777));
                    let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));
                    let mut hint_rect = RECT { left: 20, top: 440, right: 1100, bottom: 465 };
                    draw_text_str(hdc, "  Click # to move a track up • click Pause Gap to cycle 0–3 s • click Status to skip • right-click a row to remove • Space plays the album", &mut hint_rect, DT_LEFT | DT_SINGLELINE | DT_VCENTER);

                    // Album Summary Footer (y: 470..510)
                    let (total_dur, active_count, album_target) = state.engine.with_montage(|m| {
                        (m.total_duration_seconds(), m.tracks.iter().filter(|t| t.is_active).count(), m.target_album_lufs)
                    });
                    let tot_m = (total_dur / 60.0).floor() as i32;
                    let tot_s = (total_dur % 60.0).floor() as i32;
                    let cd_limit_sec = 79.0 * 60.0 + 57.0; // 79:57 Red Book CD limit
                    let cd_pct = ((total_dur / cd_limit_sec) * 100.0).clamp(0.0, 100.0) as i32;

                    let summary_str = format!(
                        "Album Summary: {} Active Tracks | Total Runtime: {:02}:{:02} | CD 80-min: {}% Used | Target: {:.1} LUFS",
                        active_count, tot_m, tot_s, cd_pct, album_target
                    );

                    let summary_rect = RECT { left: 20, top: 470, right: 1100, bottom: 505 };
                    let _ = FillRect(hdc, &summary_rect, state.panel_brush);
                    let _ = FrameRect(hdc, &summary_rect, state.meter_green_brush);
                    let _ = SetTextColor(hdc, COLORREF(0x00FF88));
                    let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
                    let mut sum_r = summary_rect;
                    draw_text_str(hdc, &format!("  {}", summary_str), &mut sum_r, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
                } else {
                    dsp_panel::paint(hdc, state);
                }
            }

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) as i16) as f32 / 120.0;
            if let Some(ref state) = GLOBAL_STATE {
                // Wheel coordinates are in screen space
                let mut pt = POINT {
                    x: (lparam.0 & 0xFFFF) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
                };
                let _ = ScreenToClient(hwnd, &mut pt);
                let dsp_visible = !state.in_stems_mode
                    && !state.engine.in_montage_mode()
                    && state.engine.processing_mode() == ProcessingMode::DspSuite;
                let fine = (wparam.0 & 0x0004) != 0; // MK_SHIFT
                let steps = if delta >= 0.0 { delta.ceil() as i32 } else { delta.floor() as i32 };
                let handled = dsp_visible && dsp_panel_gesture(state, hwnd, pt.x, pt.y, steps, fine, true);
                if !handled {
                    state.engine.with_vst3_plugin(|p| {
                        if let Some(ref plugin) = p {
                            plugin.on_wheel(delta);
                        }
                    });
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Separate the loaded track with the trained model on a worker thread, reporting progress
fn spawn_ml_separation(state: &AppState, panel_hwnd: HWND) {
    let engine = Arc::clone(&state.engine);
    let info_raw = state.info_label_hwnd.0 as usize;
    let panel_raw = panel_hwnd.0 as usize;
    std::thread::spawn(move || unsafe {
        let info = HWND(info_raw as *mut _);
        let last_pct = std::sync::atomic::AtomicI32::new(-1);
        let result = engine.separate_with_model(|p| {
            let pct = (p * 100.0) as i32;
            if last_pct.swap(pct, Ordering::Relaxed) != pct {
                set_info_text(info, &format!("Separating stems with the trained model: {}%", pct));
            }
        });
        match result {
            Ok(_) => set_info_text(info, "Stems ready: Stem Rebalance and Stem EQ now use the separated stems"),
            Err(e) => set_info_text(info, &format!("Stem separation: {}", e)),
        }
        let _ = InvalidateRect(HWND(panel_raw as *mut _), None, false);
    });
}

/// Route a click or wheel gesture on the built-in suite editor. Returns true if it hit the panel.
unsafe fn dsp_panel_gesture(state: &AppState, hwnd: HWND, x: i32, y: i32, steps: i32, fine: bool, is_wheel: bool) -> bool {
    let mut selected = state.dsp_selected.get();
    let Some(hit) = state.engine.with_dsp_suite(|s| dsp_panel::hit_test(s, selected, x, y)) else {
        return false;
    };
    if hit == dsp_panel::PanelHit::SeparateStems {
        if !is_wheel && steps > 0 {
            spawn_ml_separation(state, hwnd);
        }
        return true;
    }
    if hit == dsp_panel::PanelHit::LearnMatch {
        if !is_wheel && steps > 0 {
            match state.engine.learn_match_eq() {
                Ok(()) => set_info_text(state.info_label_hwnd, "Match EQ learned the reference's tonal balance and is now on"),
                Err(e) => set_info_text(state.info_label_hwnd, &format!("Match EQ: {}", e)),
            }
            let _ = InvalidateRect(hwnd, None, false);
        }
        return true;
    }
    if dsp_panel::edits_suite(hit, steps, is_wheel) {
        let current = selected;
        state.engine.edit_dsp_suite(move |s| dsp_panel::apply_edit(s, current, hit, steps, fine, is_wheel));
    }
    selected = dsp_panel::selection_after(hit, selected, is_wheel);
    state.dsp_selected.set(selected);
    let _ = InvalidateRect(hwnd, None, false);
    true
}

/// Montage table row under a viewport y coordinate (rows start at y=150, 32 px apart, 10 shown)
fn montage_row_at(y: i32) -> Option<usize> {
    if y < 150 {
        return None;
    }
    let idx = ((y - 150) / 32) as usize;
    if idx < 10 && (y - 150) % 32 < 30 { Some(idx) } else { None }
}

static mut IS_SCRUBBING: bool = false;

/// Custom window procedure for the interactive stereo waveform canvas & scrubber
unsafe extern "system" fn waveform_canvas_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            let mut rect: RECT = std::mem::zeroed();
            let _ = GetClientRect(hwnd, &mut rect);
            let w = (rect.right - rect.left).max(1) as f64;
            let x = ((lparam.0 & 0xFFFF) as i16 as f64).clamp(0.0, w);
            let fraction = x / w;

            let shift_down = (GetKeyState(0x10) as u16 & 0x8000) != 0;
            if shift_down {
                if let Some(ref state) = GLOBAL_STATE {
                    state.engine.set_trim_start_fraction(fraction);
                    let msg = format!("Trim IN point set: {:.1}%", fraction * 100.0);
                    let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                    unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                    let _ = InvalidateRect(hwnd, None, false);
                }
                return LRESULT(0);
            }

            let _ = SetCapture(hwnd);
            IS_SCRUBBING = true;

            if let Some(ref state) = GLOBAL_STATE {
                state.engine.seek_fraction(fraction);
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            let mut rect: RECT = std::mem::zeroed();
            let _ = GetClientRect(hwnd, &mut rect);
            let w = (rect.right - rect.left).max(1) as f64;
            let x = ((lparam.0 & 0xFFFF) as i16 as f64).clamp(0.0, w);
            let fraction = x / w;

            if let Some(ref state) = GLOBAL_STATE {
                state.engine.set_trim_end_fraction(fraction);
                let msg = format!("Trim OUT point set: {:.1}%", fraction * 100.0);
                let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if IS_SCRUBBING || (wparam.0 & 0x0001) != 0 {
                let mut rect: RECT = std::mem::zeroed();
                let _ = GetClientRect(hwnd, &mut rect);
                let w = (rect.right - rect.left).max(1) as f64;
                let x = ((lparam.0 & 0xFFFF) as i16 as f64).clamp(0.0, w);
                let fraction = x / w;

                if let Some(ref state) = GLOBAL_STATE {
                    state.engine.seek_fraction(fraction);
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if IS_SCRUBBING {
                IS_SCRUBBING = false;
                let _ = ReleaseCapture();
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);

            if let Some(ref state) = GLOBAL_STATE {
                let mut rect: RECT = std::mem::zeroed();
                let _ = GetClientRect(hwnd, &mut rect);
                let w = rect.right - rect.left;
                let h = rect.bottom - rect.top;

                // 1. Sleek waveform panel background
                let _ = FillRect(hdc, &rect, state.panel_brush);

                // Check audio buffer
                let has_audio = state.engine.with_audio_buffer(|opt_buf| {
                    if let Some(ref buf) = opt_buf {
                        let peaks = buf.peaks();
                        let num_peaks = peaks.len();
                        if num_peaks == 0 || buf.num_frames() == 0 {
                            return false;
                        }

                        let mid_y = h / 2;
                        let half_h = (h / 2 - 2).max(1) as f32;

                        // Center axis reference line
                        let center_rect = RECT { left: 0, top: mid_y, right: w, bottom: mid_y + 1 };
                        let _ = FillRect(hdc, &center_rect, state.meter_dim_brush);

                        // Render stereo waveform bars
                        let is_ref = state.engine.is_referencing();
                        for col in 0..w {
                            let peak_idx = ((col as f64 / w as f64) * num_peaks as f64) as usize;
                            if peak_idx < num_peaks {
                                let p = &peaks[peak_idx];
                                // Left channel (upper)
                                let bar_top = mid_y - (p.max_l * half_h) as i32;
                                let bar_bottom = mid_y + (p.max_r * half_h) as i32 + 1;

                                let bar_rect = RECT {
                                    left: col,
                                    top: bar_top.max(1),
                                    right: col + 1,
                                    bottom: bar_bottom.min(h - 1),
                                };

                                let brush = if is_ref {
                                    state.meter_amber_brush
                                } else if p.max_l > 0.95 || p.max_r > 0.95 {
                                    state.meter_red_brush
                                } else {
                                    state.accent_brush
                                };
                                let _ = FillRect(hdc, &bar_rect, brush);
                            }
                        }

                        // 1b. Render Trim Boundaries & Shaded Excluded Zones
                        let tf = state.engine.trim_fade();
                        let in_x = (tf.trim_start_fraction * w as f64) as i32;
                        let out_x = (tf.trim_end_fraction * w as f64) as i32;

                        if in_x > 0 {
                            let trim_in_rect = RECT { left: 0, top: 0, right: in_x, bottom: h };
                            let _ = FillRect(hdc, &trim_in_rect, state.corr_dim_brush);
                        }
                        if out_x < w {
                            let trim_out_rect = RECT { left: out_x, top: 0, right: w, bottom: h };
                            let _ = FillRect(hdc, &trim_out_rect, state.corr_dim_brush);
                        }

                        if tf.trim_start_fraction > 0.001 {
                            let marker_rect = RECT { left: in_x, top: 0, right: in_x + 2, bottom: h };
                            let _ = FillRect(hdc, &marker_rect, state.meter_amber_brush);
                        }
                        if tf.trim_end_fraction < 0.999 {
                            let marker_rect = RECT { left: (out_x - 2).max(0), top: 0, right: out_x, bottom: h };
                            let _ = FillRect(hdc, &marker_rect, state.meter_amber_brush);
                        }

                        // 1c. Render Fade-In and Fade-Out Envelope Guides
                        let total_sec = state.engine.duration_seconds();
                        if total_sec > 0.0 {
                            if tf.fade_in_seconds > 0.05 {
                                let fade_px = ((tf.fade_in_seconds / total_sec) * w as f64) as i32;
                                let old_pen = SelectObject(hdc, HGDIOBJ(state.spectrum_ref_pen.0 as _));
                                let pts = [POINT { x: in_x, y: h - 2 }, POINT { x: (in_x + fade_px).min(w), y: 2 }];
                                let _ = Polyline(hdc, &pts);
                                let _ = SelectObject(hdc, old_pen);
                            }
                            if tf.fade_out_seconds > 0.05 {
                                let fade_px = ((tf.fade_out_seconds / total_sec) * w as f64) as i32;
                                let old_pen = SelectObject(hdc, HGDIOBJ(state.spectrum_ref_pen.0 as _));
                                let pts = [POINT { x: (out_x - fade_px).max(0), y: 2 }, POINT { x: out_x, y: h - 2 }];
                                let _ = Polyline(hdc, &pts);
                                let _ = SelectObject(hdc, old_pen);
                            }
                        }

                        // Real-Time FFT Spectral Analyzer Overlay
                        let meters = state.engine.get_meter_levels();
                        let active_pen = if is_ref { state.spectrum_ref_pen } else { state.spectrum_pen };
                        let old_pen = SelectObject(hdc, HGDIOBJ(active_pen.0 as _));

                        let mut pts: Vec<POINT> = Vec::with_capacity(w as usize);
                        let min_db = -75.0f32;
                        let max_db = 0.0f32;
                        let db_range = (max_db - min_db).max(1.0);

                        for col in 0..w {
                            let t = col as f32 / (w - 1).max(1) as f32;
                            let bin_f = t * 127.0;
                            let b0 = (bin_f.floor() as usize).min(127);
                            let b1 = (b0 + 1).min(127);
                            let frac = bin_f - b0 as f32;
                            let mag_db = meters.spectrum_db[b0] * (1.0 - frac) + meters.spectrum_db[b1] * frac;

                            let norm = ((mag_db - min_db) / db_range).clamp(0.0, 1.0);
                            let y_pos = (h - 2) - (norm * (h - 4) as f32) as i32;
                            pts.push(POINT { x: col, y: y_pos.clamp(1, h - 2) });
                        }

                        if !pts.is_empty() {
                            let _ = Polyline(hdc, &pts);
                        }
                        let _ = SelectObject(hdc, old_pen);

                        // Playhead cursor
                        let cur_sec = state.engine.current_position_seconds();
                        let total_sec = state.engine.duration_seconds();
                        if total_sec > 0.0 {
                            let head_x = ((cur_sec / total_sec) * (w as f64)).clamp(0.0, (w - 1) as f64) as i32;
                            // Playhead glow bar
                            let glow_rect = RECT {
                                left: (head_x - 1).max(0),
                                top: 0,
                                right: (head_x + 2).min(w),
                                bottom: h,
                            };
                            let _ = FillRect(hdc, &glow_rect, state.meter_green_brush);

                            // Bright center needle
                            let head_rect = RECT {
                                left: head_x,
                                top: 0,
                                right: head_x + 1,
                                bottom: h,
                            };
                            let _ = FillRect(hdc, &head_rect, state.bg_brush);
                        }

                        // Reference Track Active Banner
                        if is_ref {
                            let _ = SetBkMode(hdc, TRANSPARENT);
                            let _ = SetTextColor(hdc, COLORREF(0x00D7FF)); // Gold
                            let _ = SelectObject(hdc, HGDIOBJ(state.hfont_bold.0 as _));
                            let mut badge_rect = RECT { left: 8, top: 2, right: w - 8, bottom: 20 };
                            draw_text_str(hdc, "[AUDITIONING REFERENCE TRACK]", &mut badge_rect, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
                        }
                        true
                    } else {
                        false
                    }
                });

                if !has_audio {
                    let _ = SetBkMode(hdc, TRANSPARENT);
                    let _ = SetTextColor(hdc, COLORREF(0x777777));
                    let _ = SelectObject(hdc, HGDIOBJ(state.hfont_ui.0 as _));
                    let mut txt_rect = rect;
                    draw_text_str(hdc, "Drop Audio Track Here to Master", &mut txt_rect, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
                }

                // Border frame around waveform display
                let _ = FrameRect(hdc, &rect, state.meter_dim_brush);
            }

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Custom window procedure for the dual stereo VU meter canvas
unsafe extern "system" fn meter_canvas_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);

            if let Some(ref state) = GLOBAL_STATE {
                let mut rect: RECT = std::mem::zeroed();
                let _ = GetClientRect(hwnd, &mut rect);
                let _ = FillRect(hdc, &rect, state.panel_brush);

                let meters = state.engine.get_meter_levels();

                // Map dB (-60 dB to 0 dB) to width in pixels (0 to 62 px)
                let db_to_w = |db: f32| -> i32 {
                    if db <= -60.0 {
                        0
                    } else {
                        (((db + 60.0) / 60.0) * 62.0).clamp(0.0, 62.0) as i32
                    }
                };

                let w_l = db_to_w(meters.peak_l_db);
                let w_r = db_to_w(meters.peak_r_db);

                // Background dim bars
                let bg_l = RECT { left: 3, top: 3, right: 65, bottom: 11 };
                let bg_r = RECT { left: 3, top: 13, right: 65, bottom: 21 };
                let _ = FillRect(hdc, &bg_l, state.meter_dim_brush);
                let _ = FillRect(hdc, &bg_r, state.meter_dim_brush);

                // Helper for level bar coloring
                let choose_brush = |db: f32, state: &AppState| -> HBRUSH {
                    if db > -1.0 {
                        state.meter_red_brush
                    } else if db > -12.0 {
                        state.meter_amber_brush
                    } else {
                        state.meter_green_brush
                    }
                };

                if w_l > 0 {
                    let fill_l = RECT { left: 3, top: 3, right: 3 + w_l, bottom: 11 };
                    let _ = FillRect(hdc, &fill_l, choose_brush(meters.peak_l_db, state));
                }

                if w_r > 0 {
                    let fill_r = RECT { left: 3, top: 13, right: 3 + w_r, bottom: 21 };
                    let _ = FillRect(hdc, &fill_r, choose_brush(meters.peak_r_db, state));
                }

                // True Peak Clip LEDs at end of bars (lit if True Peak >= -0.1 dBTP)
                let clip_l = RECT { left: 69, top: 3, right: 79, bottom: 11 };
                let clip_r = RECT { left: 69, top: 13, right: 79, bottom: 21 };

                let clip_l_brush = if meters.true_peak_l_dbtp >= -0.1 { state.meter_red_brush } else { state.meter_dim_brush };
                let clip_r_brush = if meters.true_peak_r_dbtp >= -0.1 { state.meter_red_brush } else { state.meter_dim_brush };

                let _ = FillRect(hdc, &clip_l, clip_l_brush);
                let _ = FillRect(hdc, &clip_r, clip_r_brush);

                // Phase Correlation Bar (-1.0 to +1.0)
                let corr_bg = RECT { left: 3, top: 24, right: 79, bottom: 32 };
                let _ = FillRect(hdc, &corr_bg, state.corr_dim_brush);

                // Center zero tick mark
                let center_tick = RECT { left: 40, top: 23, right: 42, bottom: 33 };
                let _ = FillRect(hdc, &center_tick, state.meter_dim_brush);

                let corr = meters.phase_correlation;
                if corr >= 0.0 {
                    let pw = (corr * 37.0).clamp(0.0, 37.0) as i32;
                    if pw > 0 {
                        let corr_fill = RECT { left: 41, top: 24, right: 41 + pw, bottom: 32 };
                        let _ = FillRect(hdc, &corr_fill, state.corr_green_brush);
                    }
                } else {
                    let pw = (corr.abs() * 37.0).clamp(0.0, 37.0) as i32;
                    if pw > 0 {
                        let corr_fill = RECT { left: 41 - pw, top: 24, right: 41, bottom: 32 };
                        let _ = FillRect(hdc, &corr_fill, state.corr_red_brush);
                    }
                }

                let _ = FrameRect(hdc, &rect, state.meter_dim_brush);
            }

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_DROPFILES => {
            let hdrop = HDROP(wparam.0 as _);
            let count = DragQueryFileW(hdrop, 0xFFFFFFFF, None);
            let mut dropped = Vec::new();
            for i in 0..count {
                let mut buf = [0u16; 1024];
                let n = DragQueryFileW(hdrop, i, Some(&mut buf));
                if n > 0 {
                    dropped.push(PathBuf::from(std::ffi::OsString::from_wide(&buf[..n as usize])));
                }
            }
            DragFinish(hdrop);

            if let Some(ref state) = GLOBAL_STATE {
                if state.engine.in_montage_mode() {
                    // Dropping onto the montage adds every file as an album track
                    add_montage_files(state, dropped);
                    return LRESULT(0);
                }
            }

            if let Some(path) = dropped.into_iter().next() {
                if let Some(ref mut state) = GLOBAL_STATE {
                    println!("Drag-and-Drop file loaded: {}", path.display());
                    match load_audio_file(&path) {
                        Ok(buf) => {
                            let filename = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                            let info_text = format!("{}: {:.1}s ({} Hz, {} frames)", filename, buf.duration_seconds(), buf.sample_rate, buf.num_frames());
                            let wide_info: Vec<u16> = info_text.encode_utf16().chain(Some(0)).collect();
                            let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide_info.as_ptr()));
                            state._current_filename = filename;
                            state.engine.set_audio_buffer(buf);
                            let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                            state.engine.play();
                        }
                        Err(e) => {
                            let err_msg = format!("Load Error: {}", e);
                            let wide: Vec<u16> = err_msg.encode_utf16().chain(Some(0)).collect();
                            let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr()));
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = wparam.0 as u32;
            let ctrl_down = (GetKeyState(0x11) as u16 & 0x8000) != 0; // VK_CONTROL = 0x11

            if let Some(ref mut state) = GLOBAL_STATE {
                match key {
                    0x20 => { // Spacebar: Play / Pause toggle
                        if state.engine.is_playing() {
                            state.engine.pause();
                        } else {
                            state.engine.play();
                        }
                    }
                    0x1B => { // Escape: Stop and rewind
                        state.engine.stop();
                        let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                    }
                    0x25 => { // Left Arrow: Scrub back 5s
                        let cur = state.engine.current_position_seconds();
                        let dur = state.engine.duration_seconds();
                        if dur > 0.0 {
                            let new_t = (cur - 5.0).max(0.0);
                            state.engine.seek_fraction(new_t / dur);
                            let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                        }
                    }
                    0x27 => { // Right Arrow: Scrub forward 5s
                        let cur = state.engine.current_position_seconds();
                        let dur = state.engine.duration_seconds();
                        if dur > 0.0 {
                            let new_t = (cur + 5.0).min(dur);
                            state.engine.seek_fraction(new_t / dur);
                            let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                        }
                    }
                    0x42 => { // 'B': Toggle A/B Bypass
                        let current_mode = state.engine.processing_mode();
                        if current_mode == ProcessingMode::Bypass {
                            state.engine.set_processing_mode(state.pre_bypass_mode);
                            let _ = SetWindowTextW(state.bypass_btn_hwnd, w!("A/B: Mastered"));
                        } else {
                            state.pre_bypass_mode = current_mode;
                            state.engine.set_processing_mode(ProcessingMode::Bypass);
                            let _ = SetWindowTextW(state.bypass_btn_hwnd, w!("A/B: Bypass (Dry)"));
                        }
                    }
                    0x4C => { // 'L': Toggle Loop
                        let new_loop = !state.engine.is_looping();
                        state.engine.set_loop(new_loop);
                        if new_loop {
                            let _ = SetWindowTextW(state.loop_btn_hwnd, w!("Loop: ON"));
                        } else {
                            let _ = SetWindowTextW(state.loop_btn_hwnd, w!("Loop: OFF"));
                        }
                    }
                    0x4F if ctrl_down => { // Ctrl + O: Open track
                        let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_OPEN), LPARAM(0));
                    }
                    0x45 if ctrl_down => { // Ctrl + E: Export master
                        let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_EXPORT), LPARAM(0));
                    }
                    0x53 if ctrl_down => { // Ctrl + S: Save preset
                        let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_SAVE_PRESET), LPARAM(0));
                    }
                    0x52 => { // 'R': Toggle Reference Auditioning
                        let new_ref = !state.engine.is_referencing();
                        state.engine.set_referencing(new_ref);
                        if new_ref {
                            let _ = SetWindowTextW(state.ref_toggle_btn_hwnd, w!("Ref: ON [R]"));
                        } else {
                            let _ = SetWindowTextW(state.ref_toggle_btn_hwnd, w!("Ref: OFF [R]"));
                        }
                        let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                    }
                    0x54 => { // 'T': Cycle Streaming Loudness Target
                        let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_TARGET), LPARAM(0));
                    }
                    0x4D => { // 'M': Toggle Single Track / Album Montage Mode
                        let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_MONTAGE), LPARAM(0));
                    }
                    0x53 => { // 'S': Toggle Stems Mode
                        let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_STEMS), LPARAM(0));
                    }
                    0x44 => { // 'D': Cycle export format
                        state.export_format_idx = (state.export_format_idx + 1) % EXPORT_FORMATS.len();
                        set_info_text(state.info_label_hwnd, &format!("Export format: {}", EXPORT_FORMATS[state.export_format_idx].name));
                        let _ = InvalidateRect(state.child_viewport, None, false);
                    }
                    0x47 => { // 'G': Loudness-matched A/B (processed plays at the input's loudness)
                        let on = !state.engine.ab_gain_match();
                        state.engine.set_ab_gain_match(on);
                        set_info_text(
                            state.info_label_hwnd,
                            if on {
                                "Loudness-matched A/B ON: processed audio plays at the input's loudness"
                            } else {
                                "Loudness-matched A/B OFF"
                            },
                        );
                    }
                    0x48 => { // 'H': Open WASAPI Audio Hardware Settings & Diagnostics
                        let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_DEVICE_SETTINGS), LPARAM(0));
                    }
                    0xDB => { // '[': Set Trim IN point to current playhead cursor
                        let cur = state.engine.current_position_seconds();
                        let dur = state.engine.duration_seconds();
                        if dur > 0.0 {
                            let frac = (cur / dur).clamp(0.0, 1.0);
                            state.engine.set_trim_start_fraction(frac);
                            let msg = format!("Trim IN point set: {:.2}s ({:.1}%)", cur, frac * 100.0);
                            let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                            unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                            let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                        }
                    }
                    0xDD => { // ']': Set Trim OUT point to current playhead cursor
                        let cur = state.engine.current_position_seconds();
                        let dur = state.engine.duration_seconds();
                        if dur > 0.0 {
                            let frac = (cur / dur).clamp(0.0, 1.0);
                            state.engine.set_trim_end_fraction(frac);
                            let msg = format!("Trim OUT point set: {:.2}s ({:.1}%)", cur, frac * 100.0);
                            let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                            unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                            let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                        }
                    }
                    0x46 => { // 'F': Cycle Fade Curves (0.0s -> 1.5s S-Curve -> 3.0s Exp -> 5.0s Linear -> OFF)
                        let cur_tf = state.engine.trim_fade();
                        let (new_sec, new_shape, desc) = if cur_tf.fade_in_seconds < 0.01 {
                            (1.5, FadeShape::SCurve, "1.5s (S-Curve)")
                        } else if cur_tf.fade_in_seconds < 2.0 {
                            (3.0, FadeShape::Exponential, "3.0s (Exponential)")
                        } else if cur_tf.fade_in_seconds < 4.0 {
                            (5.0, FadeShape::Linear, "5.0s (Linear)")
                        } else {
                            (0.0, FadeShape::SCurve, "OFF (0.0s)")
                        };
                        state.engine.set_fade_in_seconds(new_sec);
                        state.engine.set_fade_out_seconds(new_sec);
                        let mut updated = state.engine.trim_fade();
                        updated.fade_shape = new_shape;
                        state.engine.set_trim_fade(updated);
                        let msg = format!("Fade In/Out: {}", desc);
                        let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                        unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                        let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                    }
                    0x43 => { // 'C': Clear Trim & Fades (reset to full track, 0s fades)
                        state.engine.set_trim_start_fraction(0.0);
                        state.engine.set_trim_end_fraction(1.0);
                        state.engine.set_fade_in_seconds(0.0);
                        state.engine.set_fade_out_seconds(0.0);
                        let msg = "Trim & Fades reset to full track boundary";
                        let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                        unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                        let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                    }
                    0x41 => { // 'A': Master Assistant & Audio Profiler
                        let target = &TARGETS[state.current_target_idx];
                        match state.engine.run_master_assistant(target.target_lufs) {
                            Ok(report) => {
                                let msg = format!(
                                    "Assistant: {:.1} LUFS -> Thresh {:.1} dB, Width {:.0}%, Mono {}Hz (Crest {:.1} dB)",
                                    report.current_integrated_lufs,
                                    report.recommended_threshold_db,
                                    report.recommended_stereo_width * 100.0,
                                    report.recommended_mono_maker_hz as i32,
                                    report.crest_factor_db
                                );
                                set_info_text(state.info_label_hwnd, &msg);
                                let _ = InvalidateRect(state.child_viewport, None, true);

                                let eq_lines: String = report
                                    .recommended_eq_adjustments
                                    .iter()
                                    .map(|l| format!("   • {}\n", l))
                                    .collect();
                                let body = format!(
                                    "Track profile:\n   • Integrated: {:.1} LUFS (target {:.1} LUFS)\n   • Peak: {:.1} dBTP   RMS: {:.1} dB   Crest: {:.1} dB\n   • Sub-bass phase correlation: {:+.2}\n\nApplied to the Rust DSP suite:\n   • Maximizer threshold {:.1} dB, ceiling {:.1} dBTP\n   • Stereo width {:.0}%, mono maker below {:.0} Hz\n\nEQ moves:\n{}",
                                    report.current_integrated_lufs,
                                    report.target_integrated_lufs,
                                    report.peak_level_dbtp,
                                    report.rms_level_db,
                                    report.crest_factor_db,
                                    report.sub_bass_phase_corr,
                                    report.recommended_threshold_db,
                                    report.recommended_ceiling_db,
                                    report.recommended_stereo_width * 100.0,
                                    report.recommended_mono_maker_hz,
                                    eq_lines
                                );
                                let title_wide: Vec<u16> = "Master Assistant Report".encode_utf16().chain(Some(0)).collect();
                                let body_wide: Vec<u16> = body.encode_utf16().chain(Some(0)).collect();
                                let _ = MessageBoxW(hwnd, PCWSTR(body_wide.as_ptr()), PCWSTR(title_wide.as_ptr()), MB_OK | MB_ICONINFORMATION);
                            }
                            Err(e) => {
                                let err_msg = format!("Master Assistant: {}", e);
                                let wide: Vec<u16> = err_msg.encode_utf16().chain(Some(0)).collect();
                                unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                            }
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_APP_MIDI_CC => {
            let cc_num = (wparam.0 & 0xFF) as u8;
            let cc_val = ((wparam.0 >> 8) & 0xFF) as u8;
            let norm = cc_val as f32 / 127.0;

            if let Some(ref mut state) = GLOBAL_STATE {
                match cc_num {
                    1 => { // CC 1 (Mod Wheel): Exciter Drive / Mix
                        state.engine.edit_dsp_suite(move |s| {
                            s.exciter.drive = norm;
                            s.exciter.mix = norm.clamp(0.0, 1.0);
                        });
                        let msg = format!("MIDI CC 1 (Mod Wheel): Exciter Drive = {:.0}%", norm * 100.0);
                        let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                        unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                        let _ = InvalidateRect(state.child_viewport, None, true);
                    }
                    7 => { // CC 7 (Volume Fader): Maximizer Threshold (-24.0 dB to 0.0 dB)
                        let thresh = -24.0 * (1.0 - norm);
                        state.engine.edit_dsp_suite(move |s| {
                            s.maximizer.threshold_db = thresh;
                        });
                        let msg = format!("MIDI CC 7 (Volume): Maximizer Threshold = {:.1} dB", thresh);
                        let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                        unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                        let _ = InvalidateRect(state.child_viewport, None, true);
                    }
                    10 => { // CC 10 (Pan / Width): Stereo Width (0.0 to 2.0)
                        let width = norm * 2.0;
                        state.engine.edit_dsp_suite(move |s| {
                            s.imager.width = width;
                        });
                        let msg = format!("MIDI CC 10 (Pan): Stereo Width = {:.0}%", width * 100.0);
                        let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                        unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                        let _ = InvalidateRect(state.child_viewport, None, true);
                    }
                    64 => { // CC 64 (Sustain Pedal): Play / Pause toggle
                        if cc_val >= 64 {
                            if state.engine.is_playing() {
                                state.engine.pause();
                            } else {
                                state.engine.play();
                            }
                        }
                    }
                    80 => { // CC 80: Toggle Reference Track auditioning
                        if cc_val >= 64 {
                            let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_TOGGLE_REF), LPARAM(0));
                        }
                    }
                    81 => { // CC 81: Cycle Loudness Target
                        if cc_val >= 64 {
                            let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_TARGET), LPARAM(0));
                        }
                    }
                    82 => { // CC 82: Toggle Montage Mode
                        if cc_val >= 64 {
                            let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_MONTAGE), LPARAM(0));
                        }
                    }
                    83 => { // CC 83: Toggle Stems Mode
                        if cc_val >= 64 {
                            let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_STEMS), LPARAM(0));
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_APP_MIDI_NOTE => {
            let note = (wparam.0 & 0xFF) as u8;
            let vel = ((wparam.0 >> 8) & 0xFF) as u8;
            if vel > 0 {
                if let Some(ref mut state) = GLOBAL_STATE {
                    match note {
                        36 => { // Note C1: Play/Pause
                            if state.engine.is_playing() {
                                state.engine.pause();
                            } else {
                                state.engine.play();
                            }
                        }
                        38 => { // Note D1: Stop
                            state.engine.stop();
                            let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                        }
                        40 => { // Note E1: Toggle Reference
                            let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_TOGGLE_REF), LPARAM(0));
                        }
                        42 => { // Note F#1: Toggle Montage
                            let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_MONTAGE), LPARAM(0));
                        }
                        43 => { // Note G1: Toggle Stems Mode
                            let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_STEMS), LPARAM(0));
                        }
                        44 => { // Note G#1: Open WASAPI Audio Device Settings
                            let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BTN_DEVICE_SETTINGS), LPARAM(0));
                        }
                        _ => {}
                    }
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as usize;
            let code = ((wparam.0 >> 16) & 0xFFFF) as usize;

            if let Some(ref mut state) = GLOBAL_STATE {
                match id {
                    ID_BTN_BATCH => {
                        if state.is_rendering.load(Ordering::Relaxed) {
                            println!("Render or Batch is already active");
                            return LRESULT(0);
                        }
                        if let Some(folder_path) = rfd::FileDialog::new()
                            .set_title("Select Folder to Batch Master All Audio Files")
                            .pick_folder()
                        {
                            let mut audio_files = Vec::new();
                            if let Ok(entries) = std::fs::read_dir(&folder_path) {
                                for entry in entries.filter_map(|e| e.ok()) {
                                    let p = entry.path();
                                    if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                                        let ext_lower = ext.to_lowercase();
                                        if ["wav", "mp3", "flac", "aif", "aiff", "aac", "m4a"].contains(&ext_lower.as_str()) {
                                            audio_files.push(p);
                                        }
                                    }
                                }
                            }

                            if audio_files.is_empty() {
                                let _ = SetWindowTextW(state.info_label_hwnd, w!("No audio files found in selected folder"));
                                return LRESULT(0);
                            }

                            let out_dir = folder_path.join("Mastered");
                            let _ = std::fs::create_dir_all(&out_dir);

                            state.is_rendering.store(true, Ordering::Relaxed);
                            state.engine.pause();
                            let info_hwnd_raw = state.info_label_hwnd.0 as usize;
                            let is_rendering_flag = Arc::clone(&state.is_rendering);
                            let engine = Arc::clone(&state.engine);
                            let current_mode = state.engine.processing_mode();
                            let format = EXPORT_FORMATS[state.export_format_idx];

                            std::thread::spawn(move || {
                                let info_hwnd = HWND(info_hwnd_raw as *mut _);
                                let total = audio_files.len();
                                let mut mastered_count = 0;

                                for (idx, in_file) in audio_files.iter().enumerate() {
                                    let stem = in_file.file_stem().unwrap_or_default().to_string_lossy();
                                    let status_msg = format!("Batch [{}/{}]: {}...", idx + 1, total, stem);
                                    let wide: Vec<u16> = status_msg.encode_utf16().chain(Some(0)).collect();
                                    unsafe { let _ = SetWindowTextW(info_hwnd, PCWSTR(wide.as_ptr())); }

                                    if let Ok(buf) = load_audio_file(in_file) {
                                        let out_file = out_dir.join(format!("{}_master.wav", stem));
                                        let res = with_render_source(&engine, current_mode, |source| {
                                            let render_opts = RenderOptions {
                                                bwf_metadata: Some(BwfMetadata::default()),
                                                target_sample_rate: format.sample_rate,
                                                ..Default::default()
                                            };
                                            render_to_wav(&buf, &out_file, format.bit_depth, source, Some(render_opts), None, |_| {})
                                        });
                                        if res.is_ok() {
                                            mastered_count += 1;
                                        }
                                    }
                                }

                                is_rendering_flag.store(false, Ordering::Relaxed);
                                let done_msg = format!("Batch Done: {}/{} tracks in Mastered/", mastered_count, total);
                                let wide: Vec<u16> = done_msg.encode_utf16().chain(Some(0)).collect();
                                unsafe { let _ = SetWindowTextW(info_hwnd, PCWSTR(wide.as_ptr())); }
                            });
                        }
                    }
                    ID_BTN_LOAD_REF => {
                        if let Some(ref_path) = rfd::FileDialog::new()
                            .set_title("Open Reference Track for Mastering Comparison")
                            .add_filter("Audio Files", &["wav", "mp3", "flac", "aif", "aiff", "aac", "m4a"])
                            .pick_file()
                        {
                            match load_audio_file(&ref_path) {
                                Ok(buf) => {
                                    let filename = ref_path.file_name().unwrap_or_default().to_string_lossy().to_string();
                                    state.engine.set_reference_buffer(buf);
                                    state._reference_filename = filename.clone();
                                    match state.engine.auto_match_reference_gain() {
                                        Ok((m_lufs, r_lufs, delta_db)) => {
                                            let info = format!("Ref: {} | Main:{:.1} Ref:{:.1} LUFS (Δ{:+.1}dB)", filename, m_lufs, r_lufs, delta_db);
                                            let wide: Vec<u16> = info.encode_utf16().chain(Some(0)).collect();
                                            let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr()));
                                        }
                                        Err(_) => {
                                            let info = format!("Loaded Ref: {}", filename);
                                            let wide: Vec<u16> = info.encode_utf16().chain(Some(0)).collect();
                                            let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr()));
                                        }
                                    }
                                    state.engine.set_referencing(true);
                                    let _ = SetWindowTextW(state.ref_toggle_btn_hwnd, w!("Ref: ON [R]"));
                                    let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                                }
                                Err(e) => {
                                    let err_msg = format!("Ref Load Error: {}", e);
                                    let wide: Vec<u16> = err_msg.encode_utf16().chain(Some(0)).collect();
                                    let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr()));
                                }
                            }
                        }
                    }
                    ID_BTN_TOGGLE_REF => {
                        let new_ref = !state.engine.is_referencing();
                        state.engine.set_referencing(new_ref);
                        if new_ref {
                            let _ = SetWindowTextW(state.ref_toggle_btn_hwnd, w!("Ref: ON [R]"));
                        } else {
                            let _ = SetWindowTextW(state.ref_toggle_btn_hwnd, w!("Ref: OFF [R]"));
                        }
                        let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                    }
                    ID_BTN_MATCH_GAIN => {
                        match state.engine.auto_match_reference_gain() {
                            Ok((m_lufs, r_lufs, delta_db)) => {
                                let info = format!("Gain Matched: Main {:.1} vs Ref {:.1} LUFS (Offset {:+.1} dB)", m_lufs, r_lufs, delta_db);
                                let wide: Vec<u16> = info.encode_utf16().chain(Some(0)).collect();
                                let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr()));
                            }
                            Err(e) => {
                                let err_msg = format!("Gain Match: {}", e);
                                let wide: Vec<u16> = err_msg.encode_utf16().chain(Some(0)).collect();
                                let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr()));
                            }
                        }
                    }
                    ID_BTN_TARGET => {
                        state.current_target_idx = (state.current_target_idx + 1) % TARGETS.len();
                        let target = &TARGETS[state.current_target_idx];
                        let wide_btn: Vec<u16> = target.name.encode_utf16().chain(Some(0)).collect();
                        let _ = SetWindowTextW(state.target_btn_hwnd, PCWSTR(wide_btn.as_ptr()));

                        match state.engine.apply_loudness_target(target.target_lufs, target.ceiling_dbtp) {
                            Ok((track_lufs, boost)) => {
                                let status = format!("Target {}: {:.1} LUFS -> Drive {:+.1} dB (Ceil: {:.1} dBTP)", target.name, track_lufs, boost, target.ceiling_dbtp);
                                let wide_st: Vec<u16> = status.encode_utf16().chain(Some(0)).collect();
                                let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide_st.as_ptr()));
                            }
                            Err(_) => {
                                let status = format!("Target: {} (Ceil: {:.1} dBTP)", target.name, target.ceiling_dbtp);
                                let wide_st: Vec<u16> = status.encode_utf16().chain(Some(0)).collect();
                                let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide_st.as_ptr()));
                            }
                        }
                        let _ = InvalidateRect(state.child_viewport, None, true);
                    }
                    ID_BTN_MONTAGE => {
                        let new_mode = !state.engine.in_montage_mode();
                        state.engine.set_montage_mode(new_mode);
                        if new_mode {
                            if state.in_stems_mode {
                                state.in_stems_mode = false;
                                let _ = SetWindowTextW(state.stems_btn_hwnd, w!("Stems [S]"));
                            }
                            let _ = SetWindowTextW(state.montage_btn_hwnd, w!("Montage: ON [M]"));
                            set_info_text(state.info_label_hwnd, "Album Montage Mode Active (CUE / Red Book CD Authoring)");
                            spawn_montage_refresh(state);
                        } else {
                            let _ = SetWindowTextW(state.montage_btn_hwnd, w!("Montage [M]"));
                            set_info_text(state.info_label_hwnd, "Single Track Mastering Mode Active");
                        }
                        let _ = InvalidateRect(state.waveform_canvas_hwnd, None, true);
                        refresh_viewport_layout(state);
                    }
                    ID_BTN_STEMS => {
                        state.in_stems_mode = !state.in_stems_mode;
                        if state.in_stems_mode {
                            if state.engine.in_montage_mode() {
                                state.engine.set_montage_mode(false);
                                let _ = SetWindowTextW(state.montage_btn_hwnd, w!("Montage [M]"));
                            }
                            let _ = SetWindowTextW(state.stems_btn_hwnd, w!("Stems: ON [S]"));
                            let msg = "Stem Mastering Mixer Active — 6 Synchronized Stems with Bus Summation";
                            let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                            unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                        } else {
                            let _ = SetWindowTextW(state.stems_btn_hwnd, w!("Stems [S]"));
                            let msg = "Returned to Mastering View";
                            let wide: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                            unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                        }
                        refresh_viewport_layout(state);
                    }
                    ID_BTN_DEVICE_SETTINGS => {
                        let info = state.engine.get_device_info();
                        let mut dev_list = String::new();
                        for d in &info.available_devices {
                            dev_list.push_str(&format!("   • {}\n", d));
                        }
                        if dev_list.is_empty() {
                            dev_list = "   • Default Windows Audio Endpoint\n".to_string();
                        }
                        let body = format!(
                            "Active Audio Output Endpoint:\n   {}\n\nDriver Host Architecture:\n   {}\n\nStream Audio Parameters:\n   • Native Sample Rate: {} Hz\n   • Channel Topology: {} (Stereo Output)\n   • Sample Format: {}\n   • Hardware Buffer Latency: {:.1} ms (1024 frames)\n\nDetected System Endpoints ({} available):\n{}",
                            info.name,
                            info.host,
                            info.sample_rate,
                            info.channels,
                            info.sample_format,
                            info.latency_ms,
                            info.available_devices.len(),
                            dev_list
                        );
                        let title_wide: Vec<u16> = "WASAPI Audio Hardware Settings & Diagnostics".encode_utf16().chain(Some(0)).collect();
                        let body_wide: Vec<u16> = body.encode_utf16().chain(Some(0)).collect();
                        unsafe {
                            let _ = MessageBoxW(hwnd, PCWSTR(body_wide.as_ptr()), PCWSTR(title_wide.as_ptr()), MB_OK | MB_ICONINFORMATION);
                        }
                    }
                    ID_BTN_OPEN => {
                        if let Some(file_path) = rfd::FileDialog::new()
                            .set_title("Open Audio File for Mastering")
                            .add_filter("Audio Files", &["wav", "mp3", "flac", "aif", "aiff", "aac", "m4a"])
                            .pick_file()
                        {
                            println!("Loading audio track: {}", file_path.display());
                            match load_audio_file(&file_path) {
                                Ok(buf) => {
                                    let filename = file_path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .to_string();
                                    let info_text = format!(
                                        "{}: {:.1}s ({} Hz, {} frames)",
                                        filename,
                                        buf.duration_seconds(),
                                        buf.sample_rate,
                                        buf.num_frames()
                                    );
                                    let wide_info: Vec<u16> = info_text.encode_utf16().chain(Some(0)).collect();
                                    let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide_info.as_ptr()));
                                    state._current_filename = filename;
                                    let was_montage = state.engine.in_montage_mode();
                                    state.engine.set_audio_buffer(buf);
                                    if was_montage {
                                        // Opening a single track leaves the album montage
                                        let _ = SetWindowTextW(state.montage_btn_hwnd, w!("Montage [M]"));
                                        refresh_viewport_layout(state);
                                    }
                                    let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                                }
                                Err(e) => {
                                    let err_msg = format!("Load Error: {}", e);
                                    let wide: Vec<u16> = err_msg.encode_utf16().chain(Some(0)).collect();
                                    let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr()));
                                }
                            }
                        }
                    }
                    ID_BTN_EXPORT => {
                        if state.is_rendering.load(Ordering::Relaxed) {
                            println!("Export is already in progress...");
                            return LRESULT(0);
                        }

                        let opt_buf = state.engine.audio_buffer();
                        let buf = match opt_buf {
                            Some(b) => b,
                            None => {
                                let _ = SetWindowTextW(state.info_label_hwnd, w!("No audio loaded to export"));
                                return LRESULT(0);
                            }
                        };

                        let format = EXPORT_FORMATS[state.export_format_idx];
                        let dialog_title = format!("Export Mastered WAV — {} (press D to change)", format.name);
                        if let Some(save_path) = rfd::FileDialog::new()
                            .set_title(&dialog_title)
                            .set_file_name("master_output.wav")
                            .add_filter("WAV Audio (*.wav)", &["wav"])
                            .save_file()
                        {
                            println!("Exporting master to: {}", save_path.display());
                            state.engine.pause();
                            state.is_rendering.store(true, Ordering::Relaxed);

                            let current_mode = state.engine.processing_mode();
                            let info_hwnd_raw = state.info_label_hwnd.0 as usize;
                            let is_rendering_flag = Arc::clone(&state.is_rendering);
                            let engine = Arc::clone(&state.engine);

                            // Background worker thread for high-speed offline bounce
                            std::thread::spawn(move || {
                                let info_hwnd = HWND(info_hwnd_raw as *mut _);
                                let trim_fade = engine.trim_fade();
                                let stems = engine.ml_stems();

                                let render_res = with_render_source(&engine, current_mode, |source| {
                                    let render_opts = RenderOptions {
                                        trim_fade: Some(trim_fade),
                                        bwf_metadata: Some(BwfMetadata::default()),
                                        target_sample_rate: format.sample_rate,
                                        stems: stems.clone(),
                                        ..Default::default()
                                    };

                                    render_to_wav(
                                        &buf,
                                        &save_path,
                                        format.bit_depth,
                                        source,
                                        Some(render_opts),
                                        None,
                                        |progress| {
                                            let pct = (progress * 100.0) as i32;
                                            if pct % 10 == 0 {
                                                let progress_text = format!("Rendering master: {}%", pct);
                                                let wide: Vec<u16> = progress_text.encode_utf16().chain(Some(0)).collect();
                                                unsafe {
                                                    let _ = SetWindowTextW(info_hwnd, PCWSTR(wide.as_ptr()));
                                                }
                                            }
                                        },
                                    )
                                });

                                is_rendering_flag.store(false, Ordering::Relaxed);

                                unsafe {
                                    match render_res {
                                        Ok(_) => {
                                            let done_text = format!(
                                                "Master exported: {}",
                                                save_path.file_name().unwrap_or_default().to_string_lossy()
                                            );
                                            let wide: Vec<u16> = done_text.encode_utf16().chain(Some(0)).collect();
                                            let _ = SetWindowTextW(info_hwnd, PCWSTR(wide.as_ptr()));
                                        }
                                        Err(e) => {
                                            let err_text = format!("Export failed: {}", e);
                                            let wide: Vec<u16> = err_text.encode_utf16().chain(Some(0)).collect();
                                            let _ = SetWindowTextW(info_hwnd, PCWSTR(wide.as_ptr()));
                                        }
                                    }
                                }
                            });
                        }
                    }
                    ID_BTN_SAVE_PRESET => {
                        let has_plugin = state.engine.with_vst3_plugin(|p| p.is_some());
                        if state.engine.processing_mode() == ProcessingMode::DspSuite || !has_plugin {
                            // Built-in suite: save an XML preset that Load Preset reads back
                            if let Some(save_path) = rfd::FileDialog::new()
                                .set_title("Save Mastering Suite Preset")
                                .set_file_name("master_preset.xml")
                                .add_filter("Mastering Suite Preset (*.xml)", &["xml"])
                                .save_file()
                            {
                                let name = save_path.file_stem().unwrap_or_default().to_string_lossy().to_string();
                                let preset = state.engine.with_dsp_suite(|s| XmlPreset::from_suite(s, &name));
                                match preset.save_to_file(&save_path) {
                                    Ok(()) => set_info_text(state.info_label_hwnd, &format!("DSP preset saved: {}", name)),
                                    Err(e) => set_info_text(state.info_label_hwnd, &format!("Save preset failed: {}", e)),
                                }
                            }
                            return LRESULT(0);
                        }
                        if let Some(save_path) = rfd::FileDialog::new()
                            .set_title("Save Plugin State")
                            .set_file_name("plugin_state.vststate")
                            .add_filter("Plugin state (*.vststate)", &["vststate"])
                            .save_file()
                        {
                            println!("Saving preset to: {}", save_path.display());
                            let mut saved = false;
                            state.engine.with_vst3_plugin(|opt_plugin| {
                                if let Some(ref plugin) = opt_plugin {
                                    if let Ok(bytes) = plugin.save_state() {
                                        if std::fs::write(&save_path, bytes).is_ok() {
                                            saved = true;
                                        }
                                    }
                                }
                            });

                            if saved {
                                let done = format!("Preset saved: {}", save_path.file_name().unwrap_or_default().to_string_lossy());
                                let wide: Vec<u16> = done.encode_utf16().chain(Some(0)).collect();
                                let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr()));
                            } else {
                                let _ = SetWindowTextW(state.info_label_hwnd, w!("Save preset failed (plugin returned no state)"));
                            }
                        }
                    }
                    ID_BTN_LOAD_PRESET => {
                        if let Some(load_path) = rfd::FileDialog::new()
                            .set_title("Load Preset")
                            .add_filter("Presets (*.xml, *.vststate)", &["xml", "vststate"])
                            .pick_file()
                        {
                            println!("Loading preset from: {}", load_path.display());
                            let ext = load_path.extension().and_then(|s| s.to_str()).unwrap_or_default().to_lowercase();
                            if ext == "xml" {
                                match state.engine.load_xml_preset(&load_path) {
                                    Ok(preset) => {
                                        let comment_str = if !preset.comments.is_empty() {
                                            format!(" - {}", preset.comments)
                                        } else {
                                            String::new()
                                        };
                                        let ver_str = if !preset.plugin_ver.is_empty() {
                                            format!(" [{}]", preset.plugin_ver)
                                        } else {
                                            String::new()
                                        };
                                        let done = format!("Preset: {}{}{}", preset.name, ver_str, comment_str);
                                        let wide: Vec<u16> = done.encode_utf16().chain(Some(0)).collect();
                                        unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                                        let _ = InvalidateRect(state.child_viewport, None, true);
                                    }
                                    Err(e) => {
                                        let err_msg = format!("XML Preset Error: {}", e);
                                        let wide: Vec<u16> = err_msg.encode_utf16().chain(Some(0)).collect();
                                        unsafe { let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr())); }
                                    }
                                }
                            } else if let Ok(bytes) = std::fs::read(&load_path) {
                                let mut loaded = false;
                                state.engine.with_vst3_plugin(|opt_plugin| {
                                    if let Some(plugin) = opt_plugin {
                                        if plugin.load_state(&bytes).is_ok() {
                                            loaded = true;
                                        }
                                    }
                                });

                                if loaded {
                                    let done = format!("Preset loaded: {}", load_path.file_name().unwrap_or_default().to_string_lossy());
                                    let wide: Vec<u16> = done.encode_utf16().chain(Some(0)).collect();
                                    let _ = SetWindowTextW(state.info_label_hwnd, PCWSTR(wide.as_ptr()));
                                } else {
                                    let _ = SetWindowTextW(state.info_label_hwnd, w!("Load preset failed (Mismatch/Invalid)"));
                                }
                            }
                        }
                    }
                    ID_COMBO_MODULE => {
                        if code == CBN_SELCHANGE as usize {
                            let combo_hwnd = HWND(lparam.0 as _);
                            if state.is_rendering.load(Ordering::Relaxed) {
                                // The plugin is detached for the render; switching now would orphan it
                                let _ = SendMessageW(combo_hwnd, CB_SETCURSEL, WPARAM(state.selected_module_idx), LPARAM(0));
                                set_info_text(state.info_label_hwnd, "Wait for the render to finish before switching modules");
                                return LRESULT(0);
                            }
                            let sel = SendMessageW(combo_hwnd, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 as usize;
                            load_module_by_index(sel);
                        }
                    }
                    ID_BTN_BYPASS => {
                        let current_mode = state.engine.processing_mode();
                        if current_mode == ProcessingMode::Bypass {
                            state.engine.set_processing_mode(state.pre_bypass_mode);
                            let _ = SetWindowTextW(state.bypass_btn_hwnd, w!("A/B: Mastered"));
                        } else {
                            state.pre_bypass_mode = current_mode;
                            state.engine.set_processing_mode(ProcessingMode::Bypass);
                            let _ = SetWindowTextW(state.bypass_btn_hwnd, w!("A/B: Bypass (Dry)"));
                        }
                    }
                    ID_BTN_MODE => {
                        let current_mode = state.engine.processing_mode();
                        match current_mode {
                            ProcessingMode::Vst3 => {
                                state.engine.set_processing_mode(ProcessingMode::DspSuite);
                                let _ = SetWindowTextW(state.mode_btn_hwnd, w!("Mode: Built-in"));
                            }
                            ProcessingMode::DspSuite => {
                                state.engine.set_processing_mode(ProcessingMode::Vst3);
                                let _ = SetWindowTextW(state.mode_btn_hwnd, w!("Mode: VST3"));
                            }
                            ProcessingMode::Bypass => {
                                state.engine.set_processing_mode(ProcessingMode::Vst3);
                                let _ = SetWindowTextW(state.mode_btn_hwnd, w!("Mode: VST3"));
                                let _ = SetWindowTextW(state.bypass_btn_hwnd, w!("A/B: Mastered"));
                            }
                        }
                        refresh_viewport_layout(state);
                    }
                    ID_BTN_PLAY => {
                        state.engine.play();
                    }
                    ID_BTN_PAUSE => {
                        state.engine.pause();
                    }
                    ID_BTN_STOP => {
                        state.engine.stop();
                        let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                    }
                    ID_BTN_LOOP => {
                        let new_loop = !state.engine.is_looping();
                        state.engine.set_loop(new_loop);
                        if new_loop {
                            let _ = SetWindowTextW(state.loop_btn_hwnd, w!("Loop: ON"));
                        } else {
                            let _ = SetWindowTextW(state.loop_btn_hwnd, w!("Loop: OFF"));
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_ID_REFRESH {
                if let Some(ref state) = GLOBAL_STATE {
                    state.engine.collect_garbage();
                    let cur_sec = state.engine.current_position_seconds();
                    let total_sec = state.engine.duration_seconds();

                    // Update time display: MM:SS.SS / MM:SS.SS
                    let cur_m = (cur_sec / 60.0) as u32;
                    let cur_s = (cur_sec % 60.0) as u32;
                    let cur_cs = ((cur_sec * 100.0) % 100.0) as u32;

                    let tot_m = (total_sec / 60.0) as u32;
                    let tot_s = (total_sec % 60.0) as u32;
                    let tot_cs = ((total_sec * 100.0) % 100.0) as u32;

                    let time_str = format!(
                        "{:02}:{:02}.{:02} / {:02}:{:02}.{:02}",
                        cur_m, cur_s, cur_cs, tot_m, tot_s, tot_cs
                    );
                    let wide_time: Vec<u16> = time_str.encode_utf16().chain(Some(0)).collect();
                    let _ = SetWindowTextW(state.time_label_hwnd, PCWSTR(wide_time.as_ptr()));

                    // Update waveform playhead position and animate real-time FFT spectrum
                    if state.engine.is_playing() && !state.is_rendering.load(Ordering::Relaxed) {
                        let _ = InvalidateRect(state.waveform_canvas_hwnd, None, false);
                    }

                    // Update Loudness, True Peak, and Phase Correlation mastering meter text
                    let meters = state.engine.get_meter_levels();
                    let mut meter_str = format!(
                        "I:{:4.1} S:{:4.1} LUFS | TP:{:4.1} | Ø:{:+4.2}",
                        meters.integrated_lufs,
                        meters.short_term_lufs,
                        meters.true_peak_l_dbtp.max(meters.true_peak_r_dbtp),
                        meters.phase_correlation,
                    );
                    if state.engine.ab_gain_match() {
                        meter_str.push_str(&format!(" | Match {:+.1} dB", meters.ab_match_db));
                    }
                    let wide_meter: Vec<u16> = meter_str.encode_utf16().chain(Some(0)).collect();
                    let _ = SetWindowTextW(state.meter_label_hwnd, PCWSTR(wide_meter.as_ptr()));

                    // Refresh hardware-style VU and correlation meter canvas
                    let _ = InvalidateRect(state.meter_canvas_hwnd, None, false);
                }
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            if let Some(ref state) = GLOBAL_STATE {
                let hdc = HDC(wparam.0 as _);
                let _ = SetBkColor(hdc, COLORREF(0x1B1B1E));
                let _ = SetTextColor(hdc, COLORREF(0x00E5FF)); // Cyan mastering highlight
                return LRESULT(state.bg_brush.0 as _);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CTLCOLORBTN => {
            if let Some(ref state) = GLOBAL_STATE {
                return LRESULT(state.bg_brush.0 as _);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_ERASEBKGND => {
            if let Some(ref state) = GLOBAL_STATE {
                let hdc = HDC(wparam.0 as _);
                let mut rect: RECT = std::mem::zeroed();
                let _ = GetClientRect(hwnd, &mut rect);
                let _ = FillRect(hdc, &rect, state.bg_brush);
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
