use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

/// Broadcast Wave Format (BWF / EBU Tech 3285 Version 2) Metadata
#[derive(Clone, Debug)]
pub struct BwfMetadata {
    pub description: String,
    pub originator: String,
    pub originator_reference: String,
    pub origination_date: String, // YYYY-MM-DD
    pub origination_time: String, // HH:MM:SS
    pub time_reference: u64,      // Sample count since midnight
    pub integrated_lufs: f32,
    pub loudness_range_lu: f32,
    pub max_true_peak_dbtp: f32,
    pub max_momentary_lufs: f32,
    pub max_short_term_lufs: f32,
}

impl Default for BwfMetadata {
    fn default() -> Self {
        // Compute standard formatted date and time from SystemTime
        let duration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let total_secs = duration.as_secs();
        let days = total_secs / 86400;
        let day_secs = total_secs % 86400;
        let hours = (day_secs / 3600) % 24;
        let minutes = (day_secs / 60) % 60;
        let seconds = day_secs % 60;

        // Approximate Gregorian calendar calculation
        let mut year = 1970;
        let mut d = days;
        loop {
            let leap = if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) { 1 } else { 0 };
            let days_in_year = 365 + leap;
            if d >= days_in_year {
                d -= days_in_year;
                year += 1;
            } else {
                break;
            }
        }
        let leap = if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) { 1 } else { 0 };
        let days_in_months = [31, 28 + leap, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        let mut month = 1;
        for dim in days_in_months {
            if d >= dim {
                d -= dim;
                month += 1;
            } else {
                break;
            }
        }
        let day = d + 1;

        let date_str = format!("{:04}-{:02}-{:02}", year, month, day);
        let time_str = format!("{:02}:{:02}:{:02}", hours, minutes, seconds);

        Self {
            description: "Mastered with Mastering Studio".to_string(),
            originator: "Mastering Studio".to_string(),
            originator_reference: format!("OZ-{}{:02}{:02}{:02}{:02}{:02}", year, month, day, hours, minutes, seconds),
            origination_date: date_str,
            origination_time: time_str,
            time_reference: 0,
            integrated_lufs: -14.0,
            loudness_range_lu: 6.0,
            max_true_peak_dbtp: -1.0,
            max_momentary_lufs: -12.0,
            max_short_term_lufs: -13.5,
        }
    }
}

impl BwfMetadata {
    /// Serialize BWF `bext` (Broadcast Audio Extension) chunk payload (602 bytes)
    pub fn to_bext_payload(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(602);

        // Helper to copy ASCII string into fixed buffer
        let copy_padded = |out: &mut Vec<u8>, s: &str, max_len: usize| {
            let bytes = s.as_bytes();
            let copy_len = bytes.len().min(max_len);
            out.extend_from_slice(&bytes[..copy_len]);
            for _ in copy_len..max_len {
                out.push(0);
            }
        };

        // 1. Description (256 bytes)
        copy_padded(&mut buf, &self.description, 256);

        // 2. Originator (32 bytes)
        copy_padded(&mut buf, &self.originator, 32);

        // 3. OriginatorReference (32 bytes)
        copy_padded(&mut buf, &self.originator_reference, 32);

        // 4. OriginationDate (10 bytes: YYYY-MM-DD)
        copy_padded(&mut buf, &self.origination_date, 10);

        // 5. OriginationTime (8 bytes: HH:MM:SS)
        copy_padded(&mut buf, &self.origination_time, 8);

        // 6. TimeReference (low 32, high 32 = 8 bytes)
        let time_low = (self.time_reference & 0xFFFFFFFF) as u32;
        let time_high = (self.time_reference >> 32) as u32;
        buf.extend_from_slice(&time_low.to_le_bytes());
        buf.extend_from_slice(&time_high.to_le_bytes());

        // 7. Version = 2 (includes loudness values per EBU Tech 3285 v2) (2 bytes)
        buf.extend_from_slice(&2u16.to_le_bytes());

        // 8. UMID (64 bytes)
        buf.extend_from_slice(&[0u8; 64]);

        // 9. LoudnessValue (100 * LUFS) (2 bytes i16)
        let lufs_val = (self.integrated_lufs * 100.0).clamp(-32768.0, 32767.0) as i16;
        buf.extend_from_slice(&lufs_val.to_le_bytes());

        // 10. LoudnessRange (100 * LU) (2 bytes i16)
        let lra_val = (self.loudness_range_lu * 100.0).clamp(0.0, 32767.0) as i16;
        buf.extend_from_slice(&lra_val.to_le_bytes());

        // 11. MaxTruePeakLevel (100 * dBTP) (2 bytes i16)
        let tp_val = (self.max_true_peak_dbtp * 100.0).clamp(-32768.0, 32767.0) as i16;
        buf.extend_from_slice(&tp_val.to_le_bytes());

        // 12. MaxMomentaryLoudness (100 * LUFS) (2 bytes i16)
        let mom_val = (self.max_momentary_lufs * 100.0).clamp(-32768.0, 32767.0) as i16;
        buf.extend_from_slice(&mom_val.to_le_bytes());

        // 13. MaxShortTermLoudness (100 * LUFS) (2 bytes i16)
        let st_val = (self.max_short_term_lufs * 100.0).clamp(-32768.0, 32767.0) as i16;
        buf.extend_from_slice(&st_val.to_le_bytes());

        // 14. Reserved (180 bytes)
        buf.extend_from_slice(&[0u8; 180]);

        debug_assert_eq!(buf.len(), 602);
        buf
    }

    /// Injects BWF `bext` chunk into an existing WAV file and updates the RIFF header
    pub fn write_to_wav_file<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| format!("Failed to open WAV file for BWF injection: {}", e))?;

        let orig_len = file.metadata().map_err(|e| e.to_string())?.len();
        if orig_len < 12 {
            return Err("File too small to be a valid WAV file".to_string());
        }

        let bext_payload = self.to_bext_payload();
        let payload_len = bext_payload.len() as u32;

        // Build chunk header + payload
        let mut chunk_bytes = Vec::with_capacity(8 + bext_payload.len());
        chunk_bytes.extend_from_slice(b"bext");
        chunk_bytes.extend_from_slice(&payload_len.to_le_bytes());
        chunk_bytes.extend_from_slice(&bext_payload);

        // Seek to end and append bext chunk
        file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
        file.write_all(&chunk_bytes).map_err(|e| e.to_string())?;

        // Update RIFF total size field at bytes 4..8 (total size minus 8)
        let new_file_size = orig_len + chunk_bytes.len() as u64;
        let new_riff_len = (new_file_size - 8) as u32;

        file.seek(SeekFrom::Start(4)).map_err(|e| e.to_string())?;
        file.write_all(&new_riff_len.to_le_bytes()).map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())?;

        Ok(())
    }
}
