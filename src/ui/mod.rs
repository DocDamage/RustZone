//! Desktop UI (egui / eframe).
//!
//! `MasteringApp` owns all UI state; there are no globals. Long jobs (exports, batch, album
//! export, stem separation, montage rebuilds) run on worker threads and report through the
//! status line.

mod album_view;
mod app;
mod plugin_window;
mod stems_view;
mod suite_view;
mod widgets;

use std::path::PathBuf;

use crate::audio::ExportBitDepth;

pub use app::MasteringApp;

#[derive(Clone, Copy)]
pub struct LoudnessTarget {
    pub name: &'static str,
    pub target_lufs: f32,
    pub ceiling_dbtp: f32,
}

pub const TARGETS: [LoudnessTarget; 5] = [
    LoudnessTarget { name: "Spotify (-14 LUFS)", target_lufs: -14.0, ceiling_dbtp: -1.0 },
    LoudnessTarget { name: "Apple Music (-16 LUFS)", target_lufs: -16.0, ceiling_dbtp: -1.0 },
    LoudnessTarget { name: "YouTube (-14 LUFS)", target_lufs: -14.0, ceiling_dbtp: -1.0 },
    LoudnessTarget { name: "CD / Club (-9 LUFS)", target_lufs: -9.0, ceiling_dbtp: -0.1 },
    LoudnessTarget { name: "Broadcast (-23 LUFS)", target_lufs: -23.0, ceiling_dbtp: -1.0 },
];

#[derive(Clone, Copy)]
pub struct ExportFormat {
    pub name: &'static str,
    pub bit_depth: ExportBitDepth,
    pub sample_rate: Option<u32>,
}

/// Integer formats are dithered by the renderer
pub const EXPORT_FORMATS: [ExportFormat; 4] = [
    ExportFormat { name: "24-bit WAV, native rate", bit_depth: ExportBitDepth::Bit24, sample_rate: None },
    ExportFormat { name: "16-bit / 44.1 kHz CD WAV", bit_depth: ExportBitDepth::Bit16, sample_rate: Some(44100) },
    ExportFormat { name: "32-bit float WAV, native rate", bit_depth: ExportBitDepth::Float32, sample_rate: None },
    ExportFormat { name: "24-bit / 48 kHz WAV (video)", bit_depth: ExportBitDepth::Bit24, sample_rate: Some(48000) },
];

pub const AUDIO_EXTENSIONS: [&str; 7] = ["wav", "mp3", "flac", "aif", "aiff", "aac", "m4a"];

/// Command-line options
#[derive(Default, Debug)]
pub struct LaunchOptions {
    /// Audio file to open
    pub file: Option<PathBuf>,
    /// Save a PNG of the window after start-up and exit (documentation / UI checks)
    pub screenshot: Option<PathBuf>,
    /// View to show: "suite", "album" or "stems"
    pub view: Option<String>,
    /// Module to select in the suite view (preset key, e.g. "equalizer")
    pub module: Option<String>,
}

impl LaunchOptions {
    pub fn from_args() -> Self {
        let mut o = Self::default();
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--screenshot" => o.screenshot = args.next().map(PathBuf::from),
                "--view" => o.view = args.next(),
                "--module" => o.module = args.next(),
                _ => o.file = Some(PathBuf::from(a)),
            }
        }
        o
    }
}

pub fn run_app() -> Result<(), String> {
    let options = LaunchOptions::from_args();
    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Mastering Studio")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([1100.0, 700.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "Mastering Studio",
        native,
        Box::new(move |cc| {
            let app = MasteringApp::new(cc, options).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| e.to_string())
}

/// VST3 plugins in the standard per-machine folder and a `plugins` folder next to the executable.
/// Only used as optional fallback processors.
pub fn discover_vst3_plugins() -> Vec<PathBuf> {
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

/// Write an RGBA image as PNG
pub fn save_png(path: &std::path::Path, width: usize, height: usize, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())
}
