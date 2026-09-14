//! Provider adapters (master spec s7).
//!
//! Each adapter is three pieces: a pure request builder, a pure stream
//! decoder, and a thin HTTP client. Keeping the first two free of I/O is what
//! lets the wire protocols be tested against recorded streams rather than by
//! hoping a live call happens to exercise the right branch.

pub mod anthropic;
pub mod compat;
pub mod gemini;
pub mod http;
pub mod openai;
pub mod retry;
pub mod sse;
pub mod stream;

pub use anthropic::{AnthropicBackend, AnthropicDecoder};
pub use compat::{CompatBackend, CompatConfig, CompatDecoder};
pub use gemini::{GeminiBackend, GeminiDecoder};
pub use openai::{OpenAiBackend, OpenAiDecoder};
pub use retry::RetryPolicy;
pub use sse::{SseEvent, SseParser};
pub use stream::StreamDecoder;
