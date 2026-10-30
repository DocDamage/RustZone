# Mastering Studio

A standalone audio mastering workstation for Windows, written in Rust. Its built-in mastering suite covers EQ, dynamics, stereo, saturation, spectral and stem-aware processing, limiting, metering, referencing, album sequencing and export. It needs no external plugins. VST3 plugins can still be loaded, but only as optional fallback processors.

---

## ✨ Features

### Built-in mastering suite

A chain of 19 modules. Each one can be switched on or off independently, and the chain can be reordered freely. Every parameter is editable, and the whole chain is saved in presets.

| Module | What it does |
|---|---|
| **Equalizer (8-band dynamic)** | HP / shelves / bells / LP. Each band can be Stereo, Mid or Side, with optional dynamic gain. |
| **Vintage Passive EQ** | Passive-style program EQ: simultaneous low boost and cut, broad high boost, high shelf cut, transformer drive. |
| **Match EQ** | Learns the tonal balance of a reference track and applies the difference. Adjustable smoothing, amount and maximum correction. |
| **Stem EQ** | Separate low / mid / high EQ for vocals, bass, drums and other. |
| **Stem Rebalance** | Turn vocals, bass, drums or everything else up or down inside a finished mix. |
| **Low-End Shaper** | Adds punch to, or smooths, kick and bass transients below an adjustable cutoff. |
| **Bass Enhancer** | Tightens and boosts the sub band, adds harmonics so bass translates on small speakers, and can make the low end mono. |
| **Multiband Dynamics** | 4-band compressor with per-band threshold, ratio, attack, release and makeup. |
| **Vintage Compressor** | Feedback compressor with soft knee, program-dependent release, sidechain HPF, parallel mix, and Clean / Warm / Vari-Mu characters. |
| **Multiband Transient Shaper** | Per-band punch (accent or soften attacks) with adjustable crossovers. |
| **Transient Restore** | Brings back attacks that heavy limiting has flattened. |
| **Stereo Imager** | Overall width, 4-band width, mono-maker, and Stereoize for narrow sources. |
| **Harmonic Exciter** | Warm / Tube / Tape / Crisp saturation above an adjustable frequency. |
| **Tape Saturation** | 15 / 30 ips machine model: emphasis, hysteresis-flavoured saturation, head bump, HF roll-off, bias. |
| **Clarity** | Dynamic resonance smoothing around a focus frequency. |
| **Spectral Shaper** | Tames narrow spectral peaks (harshness, sibilance) relative to the local spectral average. |
| **Tonal Stabilizer** | Slowly steers the long-term tonal balance toward a Flat / Warm / Balanced / Bright target. |
| **Vintage Opto Limiter** | Smooth, program-dependent limiting with Analog / Tube / Modern colour and a hard ceiling. |
| **True-Peak Maximizer** | 2 ms lookahead limiter with 4× true-peak detection and adaptive release. |

**AI stem separation.** Stem Rebalance and Stem EQ start as real-time approximations (spectral stem masks). Click **Separate stems (AI)** in either module and the bundled HTDemucs model splits the track into drums, bass, other and vocals in the background (on a CPU it takes about as long as the track plays). From then on both modules work on the real stems, during playback and export, with no added latency. The stem mixer's Auto-Separate uses the same model. See [`models/README.md`](models/README.md) for the model's origin and licence.

All DSP modules are original implementations. The spectral modules (Match EQ, Stem EQ, Stem Rebalance, Spectral Shaper, Tonal Stabilizer) share one streaming FFT engine (2048-point, 75% overlap) that reconstructs the signal exactly at neutral settings. Each adds 2048 samples of latency while enabled. The total is shown in the editor and compensated on export.

### Honest comparisons

- **Loudness-matched A/B (`G`):** the processed signal plays back at the input's loudness, so a louder master can't win a comparison just by being louder. The correction is shown in the meter line (monitoring only; exports are unaffected).
- **Per-module auto gain-match:** the **Auto gain-match** button on each module measures loudness before and after it and trims its output, so tweaking or toggling a module changes tone rather than level.

### Editing the chain

- **Chain (left):** click a module to edit it, click its ON/OFF box to switch it, and turn the mouse wheel over it to move it earlier or later in the chain.
- **Parameters (right):** turn the mouse wheel over a row to change it (hold Shift for fine steps). Left-click steps up and right-click steps down; toggles flip and choices cycle.
- **Match EQ:** load a reference track (**+ Ref Track**), select Match EQ, and click **Learn from reference track**.

### Workflow

- **Transport:** play / pause / stop / loop, a waveform overview with click-to-seek and scrubbing, and a time counter.
- **Metering:** peak / RMS VU, integrated / short-term / momentary LUFS, true peak, phase correlation, and a spectrum analyzer.
- **Referencing:** A/B against a reference track, with automatic loudness matching.
- **Loudness targets:** Spotify / Apple / YouTube / CD-club / broadcast. The target sets the maximizer drive and ceiling.
- **Master Assistant (`A`):** analyses the track, sets up the suite, and shows a full report.
- **Trim & fades:** set in/out points at the playhead, with S-curve, exponential or linear fades.
- **Album montage:** sequence tracks with pregaps, balance loudness, reorder / skip / remove tracks, play the whole album through the chain, and export one mastered WAV plus a CUE sheet. Tracks at other sample rates are resampled.
- **Stem mixer:** split a track into 6 stems (or load stem files) and remix gain / pan / mute / solo.
- **Export:** 16-bit, 24-bit or 32-bit float WAV with optional sinc resampling. Integer formats get dither (noise-shaped for 16-bit, TPDF for 24-bit). Latency is compensated and BWF metadata is written. Batch-processes whole folders.
- **Presets:** `.xml` presets hold the chain order, every module's state and parameters, and any learned Match EQ curve. Factory presets in the element/param XML layout can also be imported.
- **Drag and drop** audio files onto the window. Dropped files are added to the album in montage mode.
- **MIDI control** of key parameters and transport.

### Fallback VST3 hosting (optional)

Choose **Load VST3 plugin (fallback)...** in the processor list, or pick a plugin found in `C:\Program Files\Common Files\VST3` or a `plugins` folder next to the executable. The plugin runs at the device rate with its editor docked in the window, and its state can be saved and loaded as `.vststate`. The built-in suite is always the default; nothing in the app depends on a plugin being installed.

---

## ⌨️ Keyboard shortcuts

| Key | Action |
|---|---|
| `Space` | Play / pause |
| `Esc` | Stop and rewind |
| `←` / `→` | Scrub ±5 s |
| `B` | A/B: processed vs. dry |
| `G` | Loudness-matched A/B on/off |
| `L` | Loop |
| `R` | Toggle reference track |
| `A` | Master Assistant |
| `T` | Cycle loudness target |
| `D` | Cycle export format |
| `M` | Album montage |
| `S` | Stem mixer |
| `H` | Audio device info |
| `[` / `]` | Trim in / out at playhead |
| `F` / `C` | Cycle fades / clear trim and fades |
| `Ctrl+O` / `Ctrl+E` / `Ctrl+S` | Open / export / save preset |

---

## 🚀 Building and running

Requires the Rust MSVC toolchain and [Git LFS](https://git-lfs.com) (for the model file). The first build downloads a prebuilt ONNX Runtime through the `ort` crate.

```powershell
cargo build --release
.\target\release\mastering_studio.exe
```

Pass an audio file path as the first argument to open it on launch. `install_shell_context_menu.reg` adds a "Master with Mastering Studio" entry to Explorer; edit the paths in it if your checkout is not at `D:\dev\ozone clone`.

### Tests

```powershell
cargo test -- --test-threads=1
```

The fallback-plugin tests only run when `MASTERING_STUDIO_TEST_VST3` points at a `.vst3` file; otherwise they skip. `engine_realtime_test` needs an audio output device, and `ml_separation_test` needs the model file.

### Real-time design

The audio callback owns everything it processes (suite, track, plugin, stems) and never waits on a lock. The UI keeps its own copy of the suite and sends every change through a lock-free queue; whatever the audio thread replaces is handed back to the UI thread to be freed. Meters and module readouts come back through atomics and a snapshot the audio thread only updates when nobody is reading it.

---

## 📁 Project structure

```
src/
├── audio/        buffer, decoder, lock-free CPAL engine, renderer (latency, dither, resampling),
│                 resampler, montage, stems & separator, ml_separator (HTDemucs), trim/fades, BWF
├── dsp/
│   ├── module.rs        Module trait, parameter descriptions, module registry
│   ├── stft.rs          Streaming STFT shared by the spectral modules
│   ├── core_modules.rs  Module impls for EQ, dynamics, imager, exciter, clarity, maximizer
│   ├── spectral.rs      Match EQ, Spectral Shaper, Tonal Stabilizer, Stem Rebalance, Stem EQ
│   ├── transient.rs     Low-End Shaper, Multiband Transient Shaper, Transient Restore, Bass Enhancer
│   ├── vintage.rs       Vintage Compressor, Passive EQ, Opto Limiter, Tape Saturation
│   ├── mod.rs           MasteringSuite (ordered chain), biquads, imager, exciter
│   └── ...              EQ, multiband, maximizer, clarity, dither, LUFS / true peak, FFT analyzer,
│                        assistant, XML presets
├── midi/         WinMM MIDI input
├── ui/           Win32 workstation UI; dsp_panel.rs is the suite editor
├── vst3/         Generic VST3 host (fallback plugins only)
├── lib.rs
└── main.rs
models/           htdemucs.onnx (Git LFS) and its licence
tests/
├── mastering_suite_test.rs   engine, export, montage, stems, presets, metering
├── module_suite_test.rs      behaviour of the suite's modules
├── engine_realtime_test.rs   command queue, loudness-matched A/B, auto gain
└── ml_separation_test.rs     trained stem separation and rebalance on stems
```
