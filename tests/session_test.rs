//! Undo/redo, snapshots and session files

use mastering_studio::audio::{render_to_wav, AudioBuffer, AudioEngine, ExportBitDepth, RenderSource};
use mastering_studio::dsp::{MasteringSuite, ModuleKind, XmlPreset};
use mastering_studio::session::{History, Session, SessionUiState, Snapshots};

fn preset_with_drive(drive: f32) -> XmlPreset {
    let mut s = MasteringSuite::new(48000.0);
    s.maximizer.threshold_db = -drive;
    XmlPreset::from_suite(&s, "t")
}

fn drive_of(p: &XmlPreset) -> f32 {
    let mut s = MasteringSuite::new(48000.0);
    p.apply_to_suite(&mut s);
    -s.maximizer.threshold_db
}

#[test]
fn test_history_coalesces_bursts_and_redoes() {
    let mut h = History::default();
    // A wheel burst on one control: three edits, one undo step
    h.record(preset_with_drive(0.0), "maximizer/0");
    h.record(preset_with_drive(1.0), "maximizer/0");
    h.record(preset_with_drive(2.0), "maximizer/0");
    // A different control starts a new step
    h.record(preset_with_drive(3.0), "imager/0");

    let back = h.undo(preset_with_drive(4.0)).unwrap();
    assert_eq!(drive_of(&back), 3.0);
    let back = h.undo(back).unwrap();
    assert_eq!(drive_of(&back), 0.0, "the burst should undo in one step");
    assert!(!h.can_undo());

    let fwd = h.redo(back).unwrap();
    assert_eq!(drive_of(&fwd), 3.0);
    let fwd = h.redo(fwd).unwrap();
    assert_eq!(drive_of(&fwd), 4.0);
    assert!(!h.can_redo());

    // A new edit after undoing clears the redo stack
    h.undo(fwd).unwrap();
    h.record(preset_with_drive(9.0), "eq/3");
    assert!(!h.can_redo());
}

#[test]
fn test_snapshot_slots_keep_their_own_settings() {
    let mut snaps = Snapshots::default();
    assert_eq!(snaps.active(), 0);
    // A -> B (empty): B starts as a copy of A
    assert!(snaps.switch(1, preset_with_drive(2.0)).is_none());
    assert_eq!(snaps.active(), 1);
    // Edit B to 6 dB, go back to A: A's 2 dB comes back
    let a = snaps.switch(0, preset_with_drive(6.0)).unwrap();
    assert_eq!(drive_of(&a), 2.0);
    // And B remembers 6 dB
    let b = snaps.switch(1, a).unwrap();
    assert_eq!(drive_of(&b), 6.0);
    assert!(snaps.is_filled(0) && snaps.is_filled(1) && !snaps.is_filled(2));
}

#[test]
fn test_session_round_trip() {
    let engine = match AudioEngine::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("No audio device ({}), skipping", e);
            return;
        }
    };
    // A real track on disk so the session can reload it
    let track_path = std::env::temp_dir().join("session_test_track.wav");
    let n = 48000;
    let tone: Vec<f32> = (0..n).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
    let buf = AudioBuffer::from_stereo(tone.clone(), tone, 48000);
    render_to_wav(&buf, &track_path, ExportBitDepth::Float32, RenderSource::Bypass, None, None, |_| {}).unwrap();
    engine.set_audio_buffer(mastering_studio::audio::load_audio_file(&track_path).unwrap());

    engine.edit_dsp_suite(|s| {
        s.maximizer.threshold_db = -5.5;
        s.move_module(ModuleKind::Tape, -6);
        s.set_auto_gain(ModuleKind::Exciter, true);
    });
    engine.set_trim_start_fraction(0.1);
    engine.set_ab_gain_match(true);
    let mut snaps = Snapshots::default();
    snaps.copy_to(2, preset_with_drive(8.0));

    let session_path = std::env::temp_dir().join("session_test.mstsession");
    let ui = SessionUiState { export_format: 2, loudness_target: 3, assistant_genre: 4 };
    Session::capture(&engine, &snaps, Some(&track_path), None, ui).save(&session_path).unwrap();

    // Fresh engine, restore
    drop(engine);
    let engine = AudioEngine::new().unwrap();
    let mut restored_snaps = Snapshots::default();
    let (warnings, ui_back) = Session::load(&session_path).unwrap().restore(&engine, &mut restored_snaps);
    assert!(warnings.is_empty(), "{:?}", warnings);
    assert_eq!(ui_back.export_format, 2);
    assert_eq!(ui_back.loudness_target, 3);
    assert_eq!(ui_back.assistant_genre, 4);
    assert!(engine.audio_buffer().is_some());
    assert!((engine.trim_fade().trim_start_fraction - 0.1).abs() < 1e-9);
    assert!(engine.ab_gain_match());
    assert!(restored_snaps.is_filled(2));
    engine.with_dsp_suite(|s| {
        assert!((s.maximizer.threshold_db + 5.5).abs() < 1e-4);
        assert!(s.auto_gain[ModuleKind::Exciter.index()]);
        let orig = MasteringSuite::new(48000.0).chain;
        assert_ne!(s.chain, orig, "chain order should come back");
    });
    assert!(engine.flush_commands());

    let _ = std::fs::remove_file(&track_path);
    let _ = std::fs::remove_file(&session_path);
}
