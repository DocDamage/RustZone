//! The small instruct model behind text requests ("warmer, less harsh, wider").
//!
//! It runs on the CPU through llama.cpp and is downloaded on first use rather than bundled. Output
//! is constrained by a grammar, and greedy sampling makes a request repeatable.
//!
//! Model: Qwen2.5-1.5B-Instruct by Alibaba Cloud (Apache 2.0), see models/README.md.

use std::io::{Read, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use sha2::{Digest, Sha256};

use super::ml_separator::find_model_file;

pub const MODEL_FILE: &str = "qwen2.5-1.5b-instruct-q4_k_m.gguf";
pub const MODEL_SIZE: u64 = 1_117_320_736;
const MODEL_SHA256: &str = "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e";
/// Pinned to a revision, so the checksum keeps matching
const MODEL_URL: &str = "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/91cad51170dc346986eccefdc2dd33a9da36ead9/qwen2.5-1.5b-instruct-q4_k_m.gguf";

/// Where a downloaded model lives
pub fn download_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("MasteringStudio").join("models"))
}

/// Look for the model where the bundled ones are found, then among the downloads
pub fn find_model() -> Option<PathBuf> {
    find_model_file(MODEL_FILE).or_else(|| download_dir().map(|d| d.join(MODEL_FILE)).filter(|p| p.is_file()))
}

/// Download the model and check it (blocking: run it on a worker thread). `progress` gets the
/// fraction done. A partial or corrupt download never takes the model's name.
pub fn download_model(cancel: &AtomicBool, progress: impl Fn(f32)) -> Result<PathBuf, String> {
    let dir = download_dir().ok_or("LOCALAPPDATA is not set")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {}", dir.display(), e))?;
    let target = dir.join(MODEL_FILE);
    let partial = dir.join(format!("{}.part", MODEL_FILE));
    let result = fetch(&partial, cancel, progress).and_then(|()| {
        std::fs::rename(&partial, &target).map_err(|e| format!("Could not save the model: {}", e))
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result.map(|()| target)
}

fn fetch(path: &Path, cancel: &AtomicBool, progress: impl Fn(f32)) -> Result<(), String> {
    let response = ureq::get(MODEL_URL).call().map_err(|e| format!("Download failed: {}", e))?;
    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(path).map_err(|e| format!("Could not write {}: {}", path.display(), e))?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0u8; 1 << 20];
    let mut received = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(crate::dsp::style_match::CANCELLED.to_string());
        }
        let n = reader.read(&mut chunk).map_err(|e| format!("Download failed: {}", e))?;
        if n == 0 {
            break;
        }
        file.write_all(&chunk[..n]).map_err(|e| format!("Could not write the model: {}", e))?;
        hasher.update(&chunk[..n]);
        received += n as u64;
        progress(received as f32 / MODEL_SIZE as f32);
    }
    let digest: String = hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect();
    if received != MODEL_SIZE || digest != MODEL_SHA256 {
        return Err("The download failed its checksum, so it was discarded".to_string());
    }
    Ok(())
}

/// llama.cpp's backend can only be set up once per process
fn backend() -> Result<&'static LlamaBackend, String> {
    static BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            let mut backend = LlamaBackend::init().map_err(|e| e.to_string())?;
            backend.void_logs();
            Ok(backend)
        })
        .as_ref()
        .map_err(|e| format!("Could not start the text model: {}", e))
}

pub struct TextModel {
    model: LlamaModel,
}

impl TextModel {
    pub fn load(path: &Path) -> Result<Self, String> {
        let model = LlamaModel::load_from_file(backend()?, path, &LlamaModelParams::default())
            .map_err(|e| format!("Failed to load text model {}: {}", path.display(), e))?;
        Ok(Self { model })
    }

    /// Load the downloaded model if it can be found
    pub fn load_default() -> Result<Self, String> {
        let path = find_model().ok_or("The text model has not been downloaded")?;
        Self::load(&path)
    }

    /// Continue `chat` (role and text of each message), keeping to the GBNF `grammar`. Stops at
    /// `max_tokens`, and errors with `CANCELLED` once `cancel` is set.
    pub fn generate(&self, chat: &[(&str, String)], grammar: &str, max_tokens: usize, cancel: &AtomicBool) -> Result<String, String> {
        let chat = chat
            .iter()
            .map(|(role, text)| LlamaChatMessage::new(role.to_string(), text.clone()).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let template = self.model.chat_template(None).map_err(|e| e.to_string())?;
        let prompt = self.model.apply_chat_template(&template, &chat, true).map_err(|e| e.to_string())?;
        let vocab = self.model.vocab();
        let tokens = vocab.tokenize(prompt.as_bytes(), false, true);

        // About the physical cores, as for the style model
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2) / 2;
        let size = tokens.len() + max_tokens + 8;
        let params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(size as u32))
            .with_n_batch(size as u32)
            .with_n_threads(threads.max(1) as i32)
            .with_n_threads_batch(threads.max(1) as i32);
        let mut context = self.model.new_context(backend()?, params).map_err(|e| format!("Text model: {}", e))?;

        let mut batch = LlamaBatch::new(size, 1);
        let last = tokens.len().saturating_sub(1);
        for (i, &token) in tokens.iter().enumerate() {
            batch.add(token, i as i32, &[0], i == last).map_err(|e| e.to_string())?;
        }
        context.decode(&mut batch).map_err(|e| format!("Text model: {}", e))?;

        let grammar = LlamaSampler::grammar(&self.model, grammar, "root").map_err(|e| format!("Text model grammar: {}", e))?;
        let mut sampler = LlamaSampler::chain_simple([grammar, LlamaSampler::greedy()]);
        let mut output = Vec::new();
        for position in tokens.len()..tokens.len() + max_tokens {
            if cancel.load(Ordering::Relaxed) {
                return Err(crate::dsp::style_match::CANCELLED.to_string());
            }
            let token = sampler.sample(&context, batch.n_tokens() - 1);
            if vocab.is_eog(token) {
                break;
            }
            output.extend(vocab.token_to_piece(token, false, None));
            batch.clear();
            batch.add(token, position as i32, &[0], true).map_err(|e| e.to_string())?;
            context.decode(&mut batch).map_err(|e| format!("Text model: {}", e))?;
        }
        String::from_utf8(output).map_err(|_| "Text model returned invalid text".to_string())
    }
}
