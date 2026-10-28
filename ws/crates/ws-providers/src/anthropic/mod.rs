//! Anthropic Messages API adapter (master spec s7.2).

pub mod client;
pub mod decode;
pub mod encode;

pub use client::AnthropicBackend;
pub use decode::AnthropicDecoder;
pub use encode::{API_VERSION, build_body};
