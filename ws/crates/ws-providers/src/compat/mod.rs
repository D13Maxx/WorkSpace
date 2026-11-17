//! OpenAI-compatible (Chat Completions) adapter.

pub mod client;
pub mod decode;
pub mod encode;

pub use client::{CompatBackend, CompatConfig};
pub use decode::CompatDecoder;
pub use encode::build_body;
