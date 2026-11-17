//! Google Gemini adapter.

pub mod client;
pub mod decode;
pub mod encode;

pub use client::GeminiBackend;
pub use decode::GeminiDecoder;
pub use encode::build_body;
