//! QC report measurements against signals with known answers

use mastering_studio::audio::qc::{analyze_buffer, analyze_file};
use mastering_studio::audio::{encode_buffer, AudioBuffer, ExportBitDepth, ExportCodec, QcReport, QcStatus, QcTargets};

const SR: u32 = 48000;

fn sine(seconds: f32, hz: f32, amp: f32, phase: f32) -> Vec<f32> {
    let n = (SR as f32 * seconds) as usize;
    (0..n).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / SR as f32 + phase).sin() * amp).collect()
}

fn check<'a>(r: &'a QcReport, name: &str) -> &'a mastering_studio::audio::qc::QcCheck {
    r.checks.iter().find(|c| c.name == name).unwrap_or_else(|| panic!("no {} check", name))
}

#[test]
fn test_reference_sine_reads_its_loudness() {
    // EBU Tech 3341: a 1 kHz sine at -20 dBFS in both channels reads -20 LUFS
    let s = sine(10.0, 1000.0, 0.1, 0.0);
    let buf = AudioBuffer::from_stereo(s.clone(), s, SR);
    let r = analyze_buffer(&buf, "sine", "test", QcTargets { target_lufs: Some(-20.0), ceiling_dbtp: Some(-1.0) });
    println!("{}", r.to_text());
    assert!((r.integrated_lufs + 20.0).abs() < 0.2, "integrated {}", r.integrated_lufs);
    assert!(r.loudness_range_lu < 0.5, "steady tone has no loudness range ({})", r.loudness_range_lu);
    assert!((r.true_peak_dbtp[0] + 20.0).abs() < 0.1);
    assert!((r.correlation - 1.0).abs() < 1e-4);
    assert!(r.mono_loudness_change_db.abs() < 0.1, "identical channels fold to mono unchanged");
    assert_eq!(r.overall(), QcStatus::Pass, "{}", r.summary());
}

#[test]
fn test_inter_sample_peaks_are_caught() {
    // fs/4 sine sampled 45 degrees off its crests: samples reach 0.707 of the true peak.
    // Built from the quarter-cycle index, since f32 phase drifts over long sample counts.
    let s: Vec<f32> = (0..SR as usize * 2)
        .map(|i| 1.2 * (std::f32::consts::FRAC_PI_2 * (i % 4) as f32 + std::f32::consts::FRAC_PI_4).sin())
        .collect();
    let buf = AudioBuffer::from_stereo(s.clone(), s, SR);
    let r = analyze_buffer(&buf, "isp", "test", QcTargets::default());
    let tp = r.true_peak_dbtp[0];
    let sp = r.sample_peak_dbfs[0];
    println!("sample peak {:.2} dBFS, true peak {:.2} dBTP, overs {}", sp, tp, r.true_peak_overs);
    assert!((sp - 20.0 * (1.2f32 * std::f32::consts::FRAC_1_SQRT_2).log10()).abs() < 0.05);
    assert!((tp - 20.0 * 1.2f32.log10()).abs() < 0.3, "true peak {:.2}", tp);
    assert!(r.true_peak_overs > 0);
    assert_eq!(check(&r, "True peak").status, QcStatus::Fail);
    // The samples themselves never reach full scale: no clipping
    assert_eq!(check(&r, "Clipping").status, QcStatus::Pass);
}

#[test]
fn test_clipping_and_phase_problems_fail() {
    let clipped: Vec<f32> = sine(3.0, 100.0, 2.0, 0.0).into_iter().map(|v| v.clamp(-1.0, 1.0)).collect();
    let inverted: Vec<f32> = clipped.iter().map(|v| -v).collect();
    let buf = AudioBuffer::from_stereo(clipped, inverted, SR);
    let r = analyze_buffer(&buf, "bad", "test", QcTargets::default());
    println!("{}", r.to_text());
    assert!(r.clip_events > 0);
    assert_eq!(check(&r, "Clipping").status, QcStatus::Fail);
    assert!(r.correlation < -0.99);
    assert_eq!(check(&r, "Phase").status, QcStatus::Fail);
    assert_eq!(check(&r, "Mono compatibility").status, QcStatus::Warn);
    assert_eq!(r.overall(), QcStatus::Fail);
}

#[test]
fn test_loudness_range_of_two_levels() {
    // 15 s at -20 dBFS then 15 s at -30 dBFS: about 10 LU of range
    let mut s = sine(15.0, 1000.0, 0.1, 0.0);
    s.extend(sine(15.0, 1000.0, 0.0316, 0.0));
    let buf = AudioBuffer::from_stereo(s.clone(), s, SR);
    let r = analyze_buffer(&buf, "two levels", "test", QcTargets::default());
    println!("LRA {:.1} LU, max short-term {:.1}", r.loudness_range_lu, r.max_short_term_lufs);
    assert!((r.loudness_range_lu - 10.0).abs() < 1.0, "LRA {}", r.loudness_range_lu);
    assert!((r.max_short_term_lufs + 20.0).abs() < 0.3);
}

#[test]
fn test_qc_of_an_exported_file() {
    let s = sine(4.0, 440.0, 0.5, 0.0);
    let buf = AudioBuffer::from_stereo(s.clone(), s, SR);
    let path = std::env::temp_dir().join("test_qc_export.flac");
    encode_buffer(&buf, &path, ExportCodec::Flac(ExportBitDepth::Bit24), None).unwrap();
    let r = analyze_file(&path, "FLAC 24-bit", QcTargets { target_lufs: Some(-14.0), ceiling_dbtp: Some(-1.0) }).unwrap();
    let report = r.write_beside(&path).unwrap();
    assert_eq!(report.file_name().unwrap(), "test_qc_export.qc.txt");
    let text = std::fs::read_to_string(&report).unwrap();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&report);
    assert!(text.contains("FLAC 24-bit, 48000 Hz"));
    assert!((r.duration_s - 4.0).abs() < 1e-3);
    // A -6 dBFS sine reads about -6 LUFS, far above a -14 target, while its -6 dBTP peak is under the ceiling
    assert_eq!(check(&r, "Loudness").status, QcStatus::Warn);
    assert_eq!(check(&r, "True peak").status, QcStatus::Pass);
}
