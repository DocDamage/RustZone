use std::fs::File;
use std::path::Path;
use super::buffer::AudioBuffer;

pub fn load_audio_file<P: AsRef<Path>>(path: P) -> Result<AudioBuffer, String> {
    let path_ref = path.as_ref();
    let ext = path_ref
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Fast-path for WAV using hound if available
    if ext == "wav" {
        if let Ok(buf) = load_wav_hound(path_ref) {
            return Ok(buf);
        }
    }

    // Universal decode via symphonia
    load_with_symphonia(path_ref)
}

fn load_wav_hound(path: &Path) -> Result<AudioBuffer, String> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| format!("Hound open error: {}", e))?;

    let spec = reader.spec();
    let num_channels = spec.channels as usize;
    let sample_rate = spec.sample_rate;

    if num_channels == 0 {
        return Err("WAV has 0 channels".to_string());
    }

    let mut left = Vec::new();
    let mut right = Vec::new();

    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => {
            let mut ch = 0;
            for sample in reader.samples::<f32>() {
                let s = sample.map_err(|e| e.to_string())?;
                if ch == 0 {
                    left.push(s);
                } else if ch == 1 {
                    right.push(s);
                }
                ch = (ch + 1) % num_channels;
            }
        }
        (hound::SampleFormat::Int, 16) => {
            let mut ch = 0;
            for sample in reader.samples::<i16>() {
                let s = (sample.map_err(|e| e.to_string())? as f32) / 32768.0;
                if ch == 0 {
                    left.push(s);
                } else if ch == 1 {
                    right.push(s);
                }
                ch = (ch + 1) % num_channels;
            }
        }
        (hound::SampleFormat::Int, 24) => {
            let mut ch = 0;
            for sample in reader.samples::<i32>() {
                let s = (sample.map_err(|e| e.to_string())? as f32) / 8388608.0;
                if ch == 0 {
                    left.push(s);
                } else if ch == 1 {
                    right.push(s);
                }
                ch = (ch + 1) % num_channels;
            }
        }
        (hound::SampleFormat::Int, 32) => {
            let mut ch = 0;
            for sample in reader.samples::<i32>() {
                let s = (sample.map_err(|e| e.to_string())? as f32) / 2147483648.0;
                if ch == 0 {
                    left.push(s);
                } else if ch == 1 {
                    right.push(s);
                }
                ch = (ch + 1) % num_channels;
            }
        }
        _ => {
            return Err(format!(
                "Unsupported WAV spec: {:?}, {} bits",
                spec.sample_format, spec.bits_per_sample
            ));
        }
    }

    if num_channels == 1 {
        right = left.clone();
    }

    Ok(AudioBuffer::from_stereo(left, right, sample_rate))
}

fn load_with_symphonia(path: &Path) -> Result<AudioBuffer, String> {
    use symphonia::core::audio::AudioBufferRef;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let src = File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
    let mss = MediaSourceStream::new(Box::new(src), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let meta_opts: MetadataOptions = Default::default();
    let fmt_opts: FormatOptions = Default::default();

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &fmt_opts, &meta_opts)
        .map_err(|e| format!("Unsupported audio container: {}", e))?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "No supported audio track found in file".to_string())?;

    let dec_opts: DecoderOptions = Default::default();
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &dec_opts)
        .map_err(|e| format!("Unsupported audio codec: {}", e))?;

    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .unwrap_or(44100);

    let mut left = Vec::new();
    let mut right = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => continue,
            Err(e) => return Err(format!("Error reading packet: {}", e)),
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(audio_buf_ref) => {
                match audio_buf_ref {
                    AudioBufferRef::F32(buf) => {
                        let planes = buf.planes();
                        let num_ch = planes.planes().len();
                        if num_ch == 1 {
                            for &s in planes.planes()[0] {
                                left.push(s);
                                right.push(s);
                            }
                        } else if num_ch >= 2 {
                            let pl = planes.planes()[0];
                            let pr = planes.planes()[1];
                            left.extend_from_slice(pl);
                            right.extend_from_slice(pr);
                        }
                    }
                    AudioBufferRef::S8(buf) => {
                        let planes = buf.planes();
                        let num_ch = planes.planes().len();
                        if num_ch == 1 {
                            for &s in planes.planes()[0] {
                                let f = s as f32 / 128.0;
                                left.push(f);
                                right.push(f);
                            }
                        } else if num_ch >= 2 {
                            for (&l, &r) in planes.planes()[0].iter().zip(planes.planes()[1].iter()) {
                                left.push(l as f32 / 128.0);
                                right.push(r as f32 / 128.0);
                            }
                        }
                    }
                    AudioBufferRef::U8(buf) => {
                        let planes = buf.planes();
                        let num_ch = planes.planes().len();
                        if num_ch == 1 {
                            for &s in planes.planes()[0] {
                                let f = (s as f32 - 128.0) / 128.0;
                                left.push(f);
                                right.push(f);
                            }
                        } else if num_ch >= 2 {
                            for (&l, &r) in planes.planes()[0].iter().zip(planes.planes()[1].iter()) {
                                left.push((l as f32 - 128.0) / 128.0);
                                right.push((r as f32 - 128.0) / 128.0);
                            }
                        }
                    }
                    AudioBufferRef::S16(buf) => {
                        let planes = buf.planes();
                        let num_ch = planes.planes().len();
                        if num_ch == 1 {
                            for &s in planes.planes()[0] {
                                let f = s as f32 / 32768.0;
                                left.push(f);
                                right.push(f);
                            }
                        } else if num_ch >= 2 {
                            for (&l, &r) in planes.planes()[0].iter().zip(planes.planes()[1].iter()) {
                                left.push(l as f32 / 32768.0);
                                right.push(r as f32 / 32768.0);
                            }
                        }
                    }
                    AudioBufferRef::S24(buf) => {
                        let planes = buf.planes();
                        let num_ch = planes.planes().len();
                        if num_ch == 1 {
                            for &s in planes.planes()[0] {
                                let f = s.0 as f32 / 8388608.0;
                                left.push(f);
                                right.push(f);
                            }
                        } else if num_ch >= 2 {
                            for (&l, &r) in planes.planes()[0].iter().zip(planes.planes()[1].iter()) {
                                left.push(l.0 as f32 / 8388608.0);
                                right.push(r.0 as f32 / 8388608.0);
                            }
                        }
                    }
                    AudioBufferRef::S32(buf) => {
                        let planes = buf.planes();
                        let num_ch = planes.planes().len();
                        if num_ch == 1 {
                            for &s in planes.planes()[0] {
                                let f = s as f32 / 2147483648.0;
                                left.push(f);
                                right.push(f);
                            }
                        } else if num_ch >= 2 {
                            for (&l, &r) in planes.planes()[0].iter().zip(planes.planes()[1].iter()) {
                                left.push(l as f32 / 2147483648.0);
                                right.push(r as f32 / 2147483648.0);
                            }
                        }
                    }
                    AudioBufferRef::F64(buf) => {
                        let planes = buf.planes();
                        let num_ch = planes.planes().len();
                        if num_ch == 1 {
                            for &s in planes.planes()[0] {
                                let f = s as f32;
                                left.push(f);
                                right.push(f);
                            }
                        } else if num_ch >= 2 {
                            for (&l, &r) in planes.planes()[0].iter().zip(planes.planes()[1].iter()) {
                                left.push(l as f32);
                                right.push(r as f32);
                            }
                        }
                    }
                    AudioBufferRef::U16(buf) => {
                        let planes = buf.planes();
                        let num_ch = planes.planes().len();
                        if num_ch == 1 {
                            for &s in planes.planes()[0] {
                                let f = (s as f32 - 32768.0) / 32768.0;
                                left.push(f);
                                right.push(f);
                            }
                        } else if num_ch >= 2 {
                            for (&l, &r) in planes.planes()[0].iter().zip(planes.planes()[1].iter()) {
                                left.push((l as f32 - 32768.0) / 32768.0);
                                right.push((r as f32 - 32768.0) / 32768.0);
                            }
                        }
                    }
                    AudioBufferRef::U24(buf) => {
                        let planes = buf.planes();
                        let num_ch = planes.planes().len();
                        if num_ch == 1 {
                            for &s in planes.planes()[0] {
                                let f = (s.0 as f32 - 8388608.0) / 8388608.0;
                                left.push(f);
                                right.push(f);
                            }
                        } else if num_ch >= 2 {
                            for (&l, &r) in planes.planes()[0].iter().zip(planes.planes()[1].iter()) {
                                left.push((l.0 as f32 - 8388608.0) / 8388608.0);
                                right.push((r.0 as f32 - 8388608.0) / 8388608.0);
                            }
                        }
                    }
                    AudioBufferRef::U32(buf) => {
                        let planes = buf.planes();
                        let num_ch = planes.planes().len();
                        if num_ch == 1 {
                            for &s in planes.planes()[0] {
                                let f = (s as f32 - 2147483648.0) / 2147483648.0;
                                left.push(f);
                                right.push(f);
                            }
                        } else if num_ch >= 2 {
                            for (&l, &r) in planes.planes()[0].iter().zip(planes.planes()[1].iter()) {
                                left.push((l as f32 - 2147483648.0) / 2147483648.0);
                                right.push((r as f32 - 2147483648.0) / 2147483648.0);
                            }
                        }
                    }
                }
            }
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(format!("Decode error: {}", e)),
        }
    }

    if left.is_empty() {
        return Err("Decoded 0 audio frames from file".to_string());
    }

    Ok(AudioBuffer::from_stereo(left, right, sample_rate))
}
