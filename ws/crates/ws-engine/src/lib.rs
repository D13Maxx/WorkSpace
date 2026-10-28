//! Local inference engine (master spec s8).
//!
//! Everything here is hand-written except the forward pass, which is
//! `candle_transformers`' reference `quantized_llama` behind
//! [`model::LoadedModel`]. Master spec s8.1 requires the shipped model code to
//! be ours; the reference is staged so the swap touches one module and the
//! parity suite (s9) is the gate on it.

pub mod backend;
pub mod cache;
pub mod engine;
pub mod error;
pub mod generate;
pub mod gguf;
pub mod memory;
pub mod model;
pub mod sampler;
pub mod template;
pub mod tokenizer;

pub use backend::LocalBackend;
pub use cache::{CacheAbility, CachePlan};
pub use engine::{EngineCmd, EngineHandle};
pub use error::{EngineError, Result};
pub use generate::{GenerateOptions, generate};
pub use gguf::{GgufInfo, inspect};
pub use memory::{KvDType, MemoryEstimate};
pub use model::{
    DEFAULT_CONTEXT_CAP, LoadOptions, LoadedInfo, LoadedModel, REFERENCE_MAX_POSITIONS,
};
pub use sampler::{Sampler, SamplingParams};
pub use template::{ChatTemplate, TemplateMessage};
pub use tokenizer::{EngineTokenizer, IncrementalDecoder, TokenizerSource};
