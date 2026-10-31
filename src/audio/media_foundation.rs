//! MP3 and AAC (.m4a) encoding through Windows Media Foundation's built-in encoders.
//!
//! Media Foundation is loaded at run time rather than linked, so the app still starts on
//! systems without it (Windows "N" editions without the Media Feature Pack, server images);
//! exporting a lossy format then reports that it is unavailable.

use std::ffi::c_void;
use std::path::Path;

use windows::core::{Interface, GUID, HRESULT, HSTRING, PCSTR, PCWSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaBuffer, IMFMediaType, IMFSample, IMFSinkWriter, MFAudioFormat_AAC, MFAudioFormat_MP3,
    MFAudioFormat_PCM, MFMediaType_Audio, MFTranscodeContainerType_MP3, MFTranscodeContainerType_MPEG4,
    MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE,
    MF_MT_SUBTYPE, MF_TRANSCODE_CONTAINERTYPE,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

/// MF_SDK_VERSION << 16 | MF_API_VERSION
const MF_VERSION: u32 = 0x0002_0070;
const MFSTARTUP_NOSOCKET: u32 = 1;
/// 100 ns units per second (Media Foundation timestamps)
const HNS_PER_SECOND: u64 = 10_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LossyCodec {
    Mp3,
    Aac,
}

impl LossyCodec {
    fn subtype(self) -> GUID {
        match self {
            LossyCodec::Mp3 => MFAudioFormat_MP3,
            LossyCodec::Aac => MFAudioFormat_AAC,
        }
    }

    fn container(self) -> GUID {
        match self {
            LossyCodec::Mp3 => MFTranscodeContainerType_MP3,
            LossyCodec::Aac => MFTranscodeContainerType_MPEG4,
        }
    }

    /// Bit rates the Windows encoder accepts, in kbit/s
    pub fn bitrates(self) -> &'static [u32] {
        match self {
            LossyCodec::Mp3 => &[128, 160, 192, 224, 256, 320],
            LossyCodec::Aac => &[96, 128, 160, 192],
        }
    }

    /// Sample rates the Windows encoder accepts
    pub fn sample_rates(self) -> &'static [u32] {
        match self {
            LossyCodec::Mp3 => &[32000, 44100, 48000],
            LossyCodec::Aac => &[44100, 48000],
        }
    }
}

/// What GetProcAddress returns, before casting to the real signature
type RawProc = unsafe extern "system" fn() -> isize;
type MfStartupFn = unsafe extern "system" fn(u32, u32) -> HRESULT;
type MfShutdownFn = unsafe extern "system" fn() -> HRESULT;
type MfCreateAttributesFn = unsafe extern "system" fn(*mut *mut c_void, u32) -> HRESULT;
type MfCreateMediaTypeFn = unsafe extern "system" fn(*mut *mut c_void) -> HRESULT;
type MfCreateSampleFn = unsafe extern "system" fn(*mut *mut c_void) -> HRESULT;
type MfCreateMemoryBufferFn = unsafe extern "system" fn(u32, *mut *mut c_void) -> HRESULT;
type MfCreateSinkWriterFromUrlFn = unsafe extern "system" fn(PCWSTR, *mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT;

/// The Media Foundation entry points this encoder uses, resolved from mfplat.dll / mfreadwrite.dll
struct MfApi {
    plat: HMODULE,
    readwrite: HMODULE,
    startup: MfStartupFn,
    shutdown: MfShutdownFn,
    create_attributes: MfCreateAttributesFn,
    create_media_type: MfCreateMediaTypeFn,
    create_sample: MfCreateSampleFn,
    create_memory_buffer: MfCreateMemoryBufferFn,
    create_sink_writer: MfCreateSinkWriterFromUrlFn,
}

impl MfApi {
    fn load() -> Result<Self, String> {
        const MISSING: &str = "Windows Media Foundation is not available on this system (Windows N editions need the Media Feature Pack)";
        unsafe {
            let plat = LoadLibraryW(&HSTRING::from("mfplat.dll")).map_err(|_| MISSING.to_string())?;
            let readwrite = match LoadLibraryW(&HSTRING::from("mfreadwrite.dll")) {
                Ok(m) => m,
                Err(_) => {
                    let _ = FreeLibrary(plat);
                    return Err(MISSING.to_string());
                }
            };
            let find = |module: HMODULE, name: &[u8]| GetProcAddress(module, PCSTR(name.as_ptr()));
            let api = (|| {
                Some(Self {
                    plat,
                    readwrite,
                    startup: std::mem::transmute::<RawProc, MfStartupFn>(find(plat, b"MFStartup\0")?),
                    shutdown: std::mem::transmute::<RawProc, MfShutdownFn>(find(plat, b"MFShutdown\0")?),
                    create_attributes: std::mem::transmute::<RawProc, MfCreateAttributesFn>(find(plat, b"MFCreateAttributes\0")?),
                    create_media_type: std::mem::transmute::<RawProc, MfCreateMediaTypeFn>(find(plat, b"MFCreateMediaType\0")?),
                    create_sample: std::mem::transmute::<RawProc, MfCreateSampleFn>(find(plat, b"MFCreateSample\0")?),
                    create_memory_buffer: std::mem::transmute::<RawProc, MfCreateMemoryBufferFn>(find(plat, b"MFCreateMemoryBuffer\0")?),
                    create_sink_writer: std::mem::transmute::<RawProc, MfCreateSinkWriterFromUrlFn>(find(readwrite, b"MFCreateSinkWriterFromURL\0")?),
                })
            })();
            match api {
                Some(api) => Ok(api),
                None => {
                    let _ = FreeLibrary(readwrite);
                    let _ = FreeLibrary(plat);
                    Err(MISSING.to_string())
                }
            }
        }
    }

    unsafe fn media_type(&self) -> windows::core::Result<IMFMediaType> {
        let mut raw = std::ptr::null_mut();
        (self.create_media_type)(&mut raw).ok()?;
        Ok(IMFMediaType::from_raw(raw))
    }
}

impl Drop for MfApi {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.readwrite);
            let _ = FreeLibrary(self.plat);
        }
    }
}

/// Whether this system can encode MP3 / AAC
pub fn is_available() -> bool {
    MfApi::load().is_ok()
}

fn mf_err(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |e| format!("{} ({})", what, e.message())
}

/// Streams 16-bit stereo PCM into an MP3 or AAC file
pub struct MfEncoder {
    // Field order matters: COM objects are released before Media Foundation shuts down
    writer: Option<IMFSinkWriter>,
    stream: u32,
    sample_rate: u32,
    frames_written: u64,
    started: bool,
    com_initialized: bool,
    api: MfApi,
}

// The sink writer is free-threaded (MTA); the encoder is only ever used from one thread at a time
unsafe impl Send for MfEncoder {}

impl MfEncoder {
    pub fn create(path: &Path, codec: LossyCodec, sample_rate: u32, kbps: u32) -> Result<Self, String> {
        if !codec.sample_rates().contains(&sample_rate) {
            return Err(format!("{:?} encoding needs one of {:?} Hz, not {} Hz", codec, codec.sample_rates(), sample_rate));
        }
        let api = MfApi::load()?;
        unsafe {
            let com_initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let mut enc = Self { writer: None, stream: 0, sample_rate, frames_written: 0, started: false, com_initialized, api };
            (enc.api.startup)(MF_VERSION, MFSTARTUP_NOSOCKET).ok().map_err(mf_err("Media Foundation start-up failed"))?;
            enc.started = true;

            let mut raw = std::ptr::null_mut();
            (enc.api.create_attributes)(&mut raw, 1).ok().map_err(mf_err("MFCreateAttributes"))?;
            let attrs = IMFAttributes::from_raw(raw);
            attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &codec.container()).map_err(mf_err("container type"))?;

            let _ = std::fs::remove_file(path);
            let url = HSTRING::from(path.as_os_str());
            let mut raw = std::ptr::null_mut();
            (enc.api.create_sink_writer)(PCWSTR(url.as_ptr()), std::ptr::null_mut(), attrs.as_raw(), &mut raw)
                .ok()
                .map_err(mf_err("Could not create the output file"))?;
            let writer = IMFSinkWriter::from_raw(raw);

            let out = enc.api.media_type().map_err(mf_err("MFCreateMediaType"))?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(mf_err("output type"))?;
            out.SetGUID(&MF_MT_SUBTYPE, &codec.subtype()).map_err(mf_err("output type"))?;
            out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate).map_err(mf_err("output type"))?;
            out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).map_err(mf_err("output type"))?;
            if codec == LossyCodec::Aac {
                // The AAC encoder's output type names its PCM depth; the MP3 encoder rejects it
                out.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).map_err(mf_err("output type"))?;
            }
            out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, kbps * 1000 / 8).map_err(mf_err("output type"))?;
            let stream = writer
                .AddStream(&out)
                .map_err(mf_err(&format!("The Windows {:?} encoder rejected {} kbit/s at {} Hz", codec, kbps, sample_rate)))?;

            let input = enc.api.media_type().map_err(mf_err("MFCreateMediaType"))?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).map_err(mf_err("input type"))?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).map_err(mf_err("input type"))?;
            input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate).map_err(mf_err("input type"))?;
            input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).map_err(mf_err("input type"))?;
            input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).map_err(mf_err("input type"))?;
            input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4).map_err(mf_err("input type"))?;
            input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, sample_rate * 4).map_err(mf_err("input type"))?;
            input.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1).map_err(mf_err("input type"))?;
            writer.SetInputMediaType(stream, &input, None).map_err(mf_err("The encoder does not accept 16-bit stereo PCM"))?;
            writer.BeginWriting().map_err(mf_err("BeginWriting"))?;

            enc.writer = Some(writer);
            enc.stream = stream;
            Ok(enc)
        }
    }

    /// Encode interleaved 16-bit stereo samples
    pub fn write(&mut self, interleaved: &[i16]) -> Result<(), String> {
        let frames = (interleaved.len() / 2) as u64;
        if frames == 0 {
            return Ok(());
        }
        let writer = self.writer.as_ref().ok_or("encoder already finished")?;
        let bytes = (frames * 4) as u32;
        unsafe {
            let mut raw = std::ptr::null_mut();
            (self.api.create_memory_buffer)(bytes, &mut raw).ok().map_err(mf_err("MFCreateMemoryBuffer"))?;
            let buffer = IMFMediaBuffer::from_raw(raw);
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None).map_err(mf_err("buffer lock"))?;
            std::ptr::copy_nonoverlapping(interleaved.as_ptr() as *const u8, data, bytes as usize);
            buffer.Unlock().map_err(mf_err("buffer unlock"))?;
            buffer.SetCurrentLength(bytes).map_err(mf_err("buffer length"))?;

            let mut raw = std::ptr::null_mut();
            (self.api.create_sample)(&mut raw).ok().map_err(mf_err("MFCreateSample"))?;
            let sample = IMFSample::from_raw(raw);
            sample.AddBuffer(&buffer).map_err(mf_err("AddBuffer"))?;
            let sr = self.sample_rate as u64;
            let start = self.frames_written * HNS_PER_SECOND / sr;
            let end = (self.frames_written + frames) * HNS_PER_SECOND / sr;
            sample.SetSampleTime(start as i64).map_err(mf_err("sample time"))?;
            sample.SetSampleDuration((end - start) as i64).map_err(mf_err("sample duration"))?;
            writer.WriteSample(self.stream, &sample).map_err(mf_err("Encoding failed"))?;
        }
        self.frames_written += frames;
        Ok(())
    }

    /// Flush the encoder and close the file
    pub fn finish(mut self) -> Result<(), String> {
        let writer = self.writer.take().ok_or("encoder already finished")?;
        unsafe { writer.Finalize() }.map_err(mf_err("Could not finish the file"))
    }
}

impl Drop for MfEncoder {
    fn drop(&mut self) {
        self.writer = None;
        unsafe {
            if self.started {
                let _ = (self.api.shutdown)();
            }
            if self.com_initialized {
                CoUninitialize();
            }
        }
    }
}
