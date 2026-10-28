//! GGUF inspection and validation (master spec s8.3).
//!
//! **Every GGUF file is untrusted input.** A model file is something a user
//! downloaded from the internet, and this code runs before any of it is
//! trusted enough to allocate against. So the order matters: read the header,
//! check every number in it is plausible, check every tensor lies inside the
//! file, and only then let anything else touch it.
//!
//! The failure mode being designed against is not a malicious file so much as
//! a truncated one — an interrupted download that would otherwise surface as
//! a panic deep inside a tensor read, or as an allocation for a shape whose
//! element count overflowed.

use crate::error::{EngineError, Result};
use candle_core::quantized::gguf_file;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Ceilings for header fields. Anything past these is a corrupt or hostile
/// file rather than a model anyone is going to run on a laptop.
const MAX_TENSORS: u64 = 100_000;
const MAX_METADATA_ENTRIES: u64 = 100_000;
const MAX_DIMENSIONS: usize = 8;
const MAX_ELEMENTS: u64 = 1 << 40;

/// RoPE frequency scaling a file asks for.
///
/// This matters far more than its size suggests. Master spec s8.4 calls RoPE
/// the highest-risk component precisely because getting it wrong is *quiet*:
/// the model still produces fluent text on short prompts, and only degrades as
/// the context grows. A silent quality loss is worse than a refusal, so a file
/// that needs scaling this build cannot apply says so up front.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "encoding", rename_all = "snake_case")]
pub enum RopeScaling {
    /// Per-frequency divisors precomputed into a `rope_freqs.weight` tensor.
    /// This is how llama.cpp encodes Llama 3.1 and 3.2's `llama3` rope type.
    FrequencyFactors,
    /// A scaling type declared in metadata: linear, yarn, longrope.
    Declared { kind: String, factor: Option<f32> },
}

impl std::fmt::Display for RopeScaling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FrequencyFactors => f.write_str("per-frequency factors (Llama 3.1/3.2 style)"),
            Self::Declared {
                kind,
                factor: Some(x),
            } => write!(f, "{kind} by {x}"),
            Self::Declared { kind, factor: None } => f.write_str(kind),
        }
    }
}

/// The tensor llama.cpp writes when a model needs `llama3` frequency scaling.
const ROPE_FREQS_TENSOR: &str = "rope_freqs.weight";

/// What a GGUF file says about itself, before anything is loaded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GgufInfo {
    pub path: PathBuf,
    pub file_size: u64,
    pub architecture: String,
    pub tensor_count: usize,
    /// The quantisation types present, most common first.
    pub quant_summary: String,
    pub context_length: Option<u32>,
    pub block_count: Option<u32>,
    pub embedding_length: Option<u32>,
    pub head_count: Option<u32>,
    pub head_count_kv: Option<u32>,
    pub rope_freq_base: Option<f32>,
    pub rms_norm_eps: Option<f32>,
    /// Present when the file embeds a chat template (master spec s8.8).
    pub has_chat_template: bool,
    /// Total size of the tensor data, for the memory pre-check.
    pub tensor_bytes: u64,
    /// Present when the file asks for RoPE frequency scaling. The forward pass
    /// in this build does not apply any, so this is a quality warning rather
    /// than a detail (master spec s8.4).
    pub rope_scaling: Option<RopeScaling>,
}

impl GgufInfo {
    /// Head dimension, needed to size the KV cache.
    #[must_use]
    pub fn head_dim(&self) -> Option<u32> {
        match (self.embedding_length, self.head_count) {
            (Some(e), Some(h)) if h > 0 => Some(e / h),
            _ => None,
        }
    }
}

fn meta_u32(content: &gguf_file::Content, keys: &[&str]) -> Option<u32> {
    for key in keys {
        if let Some(v) = content.metadata.get(*key) {
            if let Ok(n) = v.to_u32() {
                return Some(n);
            }
            if let Ok(n) = v.to_u64() {
                return u32::try_from(n).ok();
            }
        }
    }
    None
}

fn meta_f32(content: &gguf_file::Content, keys: &[&str]) -> Option<f32> {
    for key in keys {
        if let Some(v) = content.metadata.get(*key) {
            if let Ok(n) = v.to_f32() {
                return Some(n);
            }
        }
    }
    None
}

fn meta_string(content: &gguf_file::Content, key: &str) -> Option<String> {
    content
        .metadata
        .get(key)
        .and_then(|v| v.to_string().ok())
        .cloned()
}

/// Read and validate a GGUF header without loading tensor data.
///
/// # Errors
/// Fails if the file cannot be read, is not GGUF, declares implausible
/// header values, or describes a tensor that does not lie inside the file.
pub fn inspect(path: impl AsRef<Path>) -> Result<GgufInfo> {
    let path = path.as_ref();
    let display = path.display().to_string();

    let file = File::open(path).map_err(|source| EngineError::Io {
        path: display.clone(),
        source,
    })?;
    let file_size = file
        .metadata()
        .map_err(|source| EngineError::Io {
            path: display.clone(),
            source,
        })?
        .len();
    let mut reader = BufReader::new(file);

    // `Content::read` validates the magic and version for us; anything it
    // rejects is not a GGUF file this build can use.
    let content = gguf_file::Content::read(&mut reader).map_err(|e| EngineError::NotGguf {
        path: display.clone(),
        detail: e.to_string(),
    })?;

    validate(&content, &display, file_size)?;

    let architecture = meta_string(&content, "general.architecture").ok_or_else(|| {
        EngineError::MissingMetadata {
            path: display.clone(),
            key: "general.architecture".to_owned(),
        }
    })?;

    // Hyperparameter keys are namespaced by architecture, so the llama-family
    // names are tried alongside the generic ones.
    let arch = architecture.as_str();
    let key = |suffix: &str| format!("{arch}.{suffix}");

    let mut quants: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut tensor_bytes = 0u64;
    for info in content.tensor_infos.values() {
        *quants.entry(format!("{:?}", info.ggml_dtype)).or_default() += 1;
        tensor_bytes = tensor_bytes.saturating_add(tensor_byte_len(info));
    }
    let mut ranked: Vec<_> = quants.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let quant_summary = ranked
        .iter()
        .take(3)
        .map(|(k, n)| format!("{k}×{n}"))
        .collect::<Vec<_>>()
        .join(", ");

    Ok(GgufInfo {
        path: path.to_path_buf(),
        file_size,
        tensor_count: content.tensor_infos.len(),
        quant_summary,
        context_length: meta_u32(&content, &[&key("context_length")]),
        block_count: meta_u32(&content, &[&key("block_count")]),
        embedding_length: meta_u32(&content, &[&key("embedding_length")]),
        head_count: meta_u32(&content, &[&key("attention.head_count")]),
        head_count_kv: meta_u32(&content, &[&key("attention.head_count_kv")]),
        rope_freq_base: meta_f32(&content, &[&key("rope.freq_base")]),
        rms_norm_eps: meta_f32(&content, &[&key("attention.layer_norm_rms_epsilon")]),
        has_chat_template: content.metadata.contains_key("tokenizer.chat_template"),
        rope_scaling: rope_scaling(&content, &key("rope.scaling.type")),
        architecture,
        tensor_bytes,
    })
}

/// Detect RoPE scaling the forward pass would ignore.
///
/// Two encodings, because converters use both. Llama 3.1 and 3.2 have their
/// factors precomputed into a tensor; linear and YARN scaling are declared in
/// metadata instead.
fn rope_scaling(content: &gguf_file::Content, type_key: &str) -> Option<RopeScaling> {
    if content.tensor_infos.contains_key(ROPE_FREQS_TENSOR) {
        return Some(RopeScaling::FrequencyFactors);
    }
    let kind = content
        .metadata
        .get(type_key)
        .and_then(|v| v.to_string().ok())?
        .clone();
    // "none" is the honest absence of scaling, not a kind of it.
    if kind.is_empty() || kind.eq_ignore_ascii_case("none") {
        return None;
    }
    Some(RopeScaling::Declared {
        factor: meta_f32(content, &[&type_key.replace(".type", ".factor")]),
        kind,
    })
}

/// Bytes a tensor occupies, from its shape and quantisation.
fn tensor_byte_len(info: &gguf_file::TensorInfo) -> u64 {
    let elements: u64 = info
        .shape
        .dims()
        .iter()
        .try_fold(1u64, |acc, d| acc.checked_mul(*d as u64))
        .unwrap_or(0);
    let block = info.ggml_dtype.block_size() as u64;
    let type_size = info.ggml_dtype.type_size() as u64;
    if block == 0 {
        return 0;
    }
    elements / block * type_size
}

/// Check every header number is plausible and every tensor lies in the file.
///
/// Runs before any allocation: a shape whose product overflows would otherwise
/// become an allocation request derived from attacker-controlled arithmetic.
fn validate(content: &gguf_file::Content, path: &str, file_size: u64) -> Result<()> {
    let tensors = content.tensor_infos.len() as u64;
    if tensors > MAX_TENSORS {
        return Err(EngineError::ImplausibleHeader {
            path: path.to_owned(),
            field: "tensor count",
            value: tensors,
            max: MAX_TENSORS,
        });
    }
    let entries = content.metadata.len() as u64;
    if entries > MAX_METADATA_ENTRIES {
        return Err(EngineError::ImplausibleHeader {
            path: path.to_owned(),
            field: "metadata entry count",
            value: entries,
            max: MAX_METADATA_ENTRIES,
        });
    }

    for (name, info) in &content.tensor_infos {
        let dims = info.shape.dims();
        if dims.len() > MAX_DIMENSIONS {
            return Err(EngineError::ImplausibleHeader {
                path: path.to_owned(),
                field: "tensor rank",
                value: dims.len() as u64,
                max: MAX_DIMENSIONS as u64,
            });
        }

        // Checked arithmetic: a crafted shape must not wrap into a small
        // number and then be trusted (master spec s8.3).
        let elements = dims
            .iter()
            .try_fold(1u64, |acc, d| acc.checked_mul(*d as u64))
            .ok_or_else(|| EngineError::ShapeOverflow {
                name: name.clone(),
                shape: dims.to_vec(),
            })?;
        if elements > MAX_ELEMENTS {
            return Err(EngineError::ImplausibleHeader {
                path: path.to_owned(),
                field: "tensor element count",
                value: elements,
                max: MAX_ELEMENTS,
            });
        }

        let start = content
            .tensor_data_offset
            .checked_add(info.offset)
            .ok_or_else(|| EngineError::ShapeOverflow {
                name: name.clone(),
                shape: dims.to_vec(),
            })?;
        let end =
            start
                .checked_add(tensor_byte_len(info))
                .ok_or_else(|| EngineError::ShapeOverflow {
                    name: name.clone(),
                    shape: dims.to_vec(),
                })?;
        if end > file_size {
            return Err(EngineError::TensorOutOfBounds {
                name: name.clone(),
                start,
                end,
                file_size,
            });
        }
    }
    Ok(())
}

/// Reject an architecture this build has no model code for (master spec s8.3).
///
/// # Errors
/// Fails for anything but `llama`.
pub fn require_supported(info: &GgufInfo) -> Result<()> {
    if info.architecture == "llama" {
        return Ok(());
    }
    Err(EngineError::UnsupportedArchitecture(
        info.path.display().to_string(),
        info.architecture.clone(),
    ))
}

/// SHA-256 of a file, for the verification record (master spec s9).
///
/// Streams rather than reading the whole file: these are gigabytes.
///
/// # Errors
/// Fails if the file cannot be read.
pub fn file_sha256(path: impl AsRef<Path>) -> Result<String> {
    use sha2::{Digest, Sha256};
    let path = path.as_ref();
    let display = path.display().to_string();
    let mut file = File::open(path).map_err(|source| EngineError::Io {
        path: display.clone(),
        source,
    })?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| EngineError::Io {
            path: display.clone(),
            source,
        })?;

    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        use std::io::Read;
        let n = file.read(&mut buf).map_err(|source| EngineError::Io {
            path: display.clone(),
            source,
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}
