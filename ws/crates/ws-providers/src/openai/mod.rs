//! OpenAI Responses API adapter (master spec s7.3).

pub mod client;
pub mod decode;
pub mod encode;

pub use client::OpenAiBackend;
pub use decode::OpenAiDecoder;
pub use encode::build_body;
