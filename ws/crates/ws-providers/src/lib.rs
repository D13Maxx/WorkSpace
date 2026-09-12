//! Provider adapters (master spec s7).

pub mod anthropic;
pub mod sse;

pub use anthropic::AnthropicDecoder;
pub use sse::{SseEvent, SseParser};
