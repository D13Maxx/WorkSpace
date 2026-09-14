//! Local inference engine (master spec s8).

pub mod error;
pub mod gguf;
pub mod sampler;

pub use error::{EngineError, Result};
pub use gguf::{GgufInfo, inspect};
pub use sampler::{Sampler, SamplingParams};
