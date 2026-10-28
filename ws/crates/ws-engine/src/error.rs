//! Engine failures.
//!
//! A GGUF file is untrusted input (master spec s8.3): every rejection here is
//! an error with numbers in it, never a panic and never a silent truncation.
//!
//! The messages are written for the person who chose the model, not for a log
//! reader. "Not enough memory" alone leaves them nothing to change, so the
//! refusals carry the gap and the setting that closes it.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("could not read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not a GGUF file, or its header is corrupt: {detail}")]
    NotGguf { path: String, detail: String },

    #[error(
        "{path} declares a {field} of {value}, which is outside the range this build accepts (max {max})"
    )]
    ImplausibleHeader {
        path: String,
        field: &'static str,
        value: u64,
        max: u64,
    },

    #[error("tensor {name} claims bytes {start}..{end}, past the end of a {file_size}-byte file")]
    TensorOutOfBounds {
        name: String,
        start: u64,
        end: u64,
        file_size: u64,
    },

    #[error("tensor {name} has shape {shape:?}, whose element count overflows")]
    ShapeOverflow { name: String, shape: Vec<usize> },

    #[error("this build supports the llama architecture only; {0} declares {1}")]
    UnsupportedArchitecture(String, String),

    #[error("{path} is missing the required metadata key {key}")]
    MissingMetadata { path: String, key: String },

    #[error(
        "{model} needs about {needed_mib} MiB at a context cap of {context_cap}, but only {available_mib} MiB of memory is free.{}",
        suggested_cap.map_or_else(
            || " No context cap small enough would fit; this model is too large for this machine.".to_owned(),
            |cap| format!(" A context cap of {cap} would fit."),
        )
    )]
    NotEnoughMemory {
        model: String,
        needed_mib: u64,
        available_mib: u64,
        context_cap: u32,
        /// The largest offered cap that would fit, when there is one.
        suggested_cap: Option<u32>,
    },

    #[error("no tokenizer could be built from {path}: {detail}")]
    NoTokenizer { path: String, detail: String },

    #[error("the text could not be tokenized: {detail}")]
    TokenizeFailed { detail: String },

    #[error("{path} has no chat template, and this build will not guess one")]
    NoChatTemplate { path: String },

    #[error("the chat template failed to render: {0}")]
    TemplateRender(String),

    #[error(
        "a context cap of {requested} was asked for, but the model in this build runs to {supported} positions; load with a cap of {supported} or less"
    )]
    ContextCapTooLarge { requested: u32, supported: u32 },

    #[error("no model is loaded")]
    NotLoaded,

    #[error(
        "the engine panicked and has been unloaded; the app is still running. Reload the model to continue."
    )]
    Poisoned,

    #[error("the prompt is {prompt_tokens} tokens, longer than the {context_cap}-token cap")]
    PromptTooLong {
        prompt_tokens: usize,
        context_cap: u32,
    },

    #[error("generation was cancelled")]
    Cancelled,

    #[error("engine error: {0}")]
    Candle(String),

    #[error("the engine worker has stopped")]
    WorkerGone,
}

impl From<candle_core::Error> for EngineError {
    fn from(e: candle_core::Error) -> Self {
        Self::Candle(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, EngineError>;

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_memory_refusal_names_a_cap_that_would_fit() {
        let err = EngineError::NotEnoughMemory {
            model: "llama.gguf".to_owned(),
            needed_mib: 9_000,
            available_mib: 4_000,
            context_cap: 8_192,
            suggested_cap: Some(2_048),
        };
        let msg = err.to_string();
        assert!(msg.contains("9000 MiB"), "{msg}");
        assert!(msg.contains("4000 MiB"), "{msg}");
        assert!(
            msg.contains("context cap of 2048 would fit"),
            "the user needs somewhere to go: {msg}"
        );
    }

    #[test]
    fn a_hopeless_load_says_so_rather_than_suggesting_nothing() {
        let err = EngineError::NotEnoughMemory {
            model: "huge.gguf".to_owned(),
            needed_mib: 900_000,
            available_mib: 4_000,
            context_cap: 8_192,
            suggested_cap: None,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("too large for this machine"),
            "silence would read as a bug: {msg}"
        );
    }
}
