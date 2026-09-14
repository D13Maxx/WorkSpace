//! OpenAI-compatible (Chat Completions) adapter (master spec s7.5).

pub mod client;
pub mod decode;
pub mod encode;

pub use client::{CompatBackend, CompatConfig};
pub use decode::CompatDecoder;
pub use encode::build_body;
