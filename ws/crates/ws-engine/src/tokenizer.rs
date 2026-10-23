//! Tokenizer loading and incremental decoding (master spec s8.7).
//!
//! Hand-written around `tokenizers`: the vocabulary comes from the file, but
//! every policy decision here -- which stop tokens count, how BOS is handled,
//! when a partial token is safe to show -- is ours and survives the swap to
//! the hand-written model.
//!
//! The BOS rule is the one worth stating plainly. Candle's GGUF tokenizer
//! installs a `TemplateProcessing` post-processor when the file sets
//! `add_bos_token`, so encoding with special tokens enabled prepends BOS. Chat
//! templates *also* emit a begin-of-text token of their own. Encoding rendered
//! template output with specials enabled therefore produces two, which shifts
//! every position by one and quietly degrades the model. So rendered prompts
//! are always encoded with specials disabled, and BOS is added here, once,
//! only if it is not already there.

use crate::error::{EngineError, Result};
use candle_core::quantized::gguf_file;
use candle_core::quantized::tokenizer::TokenizerFromGguf;
use std::collections::BTreeSet;
use std::path::Path;
use tokenizers::Tokenizer;

/// Tokens that end a turn, beyond the model's own EOS.
///
/// Llama 3 templates close a turn with `<|eot_id|>` rather than the EOS token
/// in `tokenizer.ggml.eos_token_id`, so a generation that only watched EOS
/// would run past the end of the reply and start a new turn on the user's
/// behalf. These are looked up by string and ignored when absent.
const END_OF_TURN_TOKENS: &[&str] = &[
    "<|eot_id|>",
    "<|end_of_text|>",
    "<|im_end|>",
    "<|end|>",
    "<end_of_turn>",
    "<|endoftext|>",
];

/// How many undecodable tokens may be held back before giving up and emitting
/// what we have. Bounds the wait for a UTF-8 sequence that will never complete.
const MAX_PENDING: usize = 8;

#[derive(Debug)]
pub struct EngineTokenizer {
    inner: Tokenizer,
    bos: Option<u32>,
    stops: BTreeSet<u32>,
    /// Where the vocabulary came from, for the load report.
    source: TokenizerSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenizerSource {
    /// Built from the GGUF's own metadata.
    Gguf,
    /// A `tokenizer.json` the user supplied alongside the model.
    SidecarFile,
}

impl EngineTokenizer {
    /// Build from GGUF metadata, falling back to a `tokenizer.json` beside the
    /// model (master spec s8.7).
    ///
    /// The fallback is not a nicety: candle's GGUF tokenizer handles byte-level
    /// BPE (`tokenizer.ggml.model` of `gpt2`) only, so SentencePiece-based
    /// files -- which includes most Llama 2-era GGUFs -- reach this path.
    ///
    /// # Errors
    /// Fails when neither source yields a tokenizer, naming both attempts.
    pub fn load(content: &gguf_file::Content, model_path: &Path) -> Result<Self> {
        let display = model_path.display().to_string();

        let (inner, source) = match Tokenizer::from_gguf(content) {
            Ok(t) => (t, TokenizerSource::Gguf),
            Err(gguf_err) => {
                let sidecar = model_path.with_file_name("tokenizer.json");
                match Tokenizer::from_file(&sidecar) {
                    Ok(t) => (t, TokenizerSource::SidecarFile),
                    Err(file_err) => {
                        return Err(EngineError::NoTokenizer {
                            path: display,
                            detail: format!(
                                "its own metadata could not be used ({gguf_err}), and {} could \
                                 not be read either ({file_err}). Download tokenizer.json from \
                                 the original model repository and put it beside the .gguf file.",
                                sidecar.display()
                            ),
                        });
                    }
                }
            }
        };

        let bos = meta_token_id(content, "tokenizer.ggml.bos_token_id");
        let mut stops = BTreeSet::new();
        if let Some(eos) = meta_token_id(content, "tokenizer.ggml.eos_token_id") {
            stops.insert(eos);
        }
        // Newer converters emit this separately from EOS.
        if let Some(eot) = meta_token_id(content, "tokenizer.ggml.eot_token_id") {
            stops.insert(eot);
        }
        for name in END_OF_TURN_TOKENS {
            if let Some(id) = inner.token_to_id(name) {
                stops.insert(id);
            }
        }

        Ok(Self {
            inner,
            bos,
            stops,
            source,
        })
    }

    #[must_use]
    pub fn source(&self) -> TokenizerSource {
        self.source
    }

    #[must_use]
    pub fn vocab_size(&self) -> usize {
        self.inner.get_vocab_size(true)
    }

    #[must_use]
    pub fn is_stop(&self, id: u32) -> bool {
        self.stops.contains(&id)
    }

    /// Encode text rendered by the chat template.
    ///
    /// Special tokens are disabled, because the template has already written
    /// them as text and the tokenizer would add a second BOS on top (see the
    /// module note). Exactly one BOS is guaranteed when the model uses one.
    ///
    /// # Errors
    /// Fails if the text cannot be tokenized.
    pub fn encode_rendered(&self, text: &str) -> Result<Vec<u32>> {
        let encoding = self
            .inner
            .encode(text, false)
            .map_err(|e| EngineError::TokenizeFailed {
                detail: e.to_string(),
            })?;
        let mut ids = encoding.get_ids().to_vec();

        if let Some(bos) = self.bos {
            if ids.first() != Some(&bos) {
                ids.insert(0, bos);
            }
        }
        Ok(ids)
    }

    /// Encode plain text with no special tokens at all. Used for counting.
    ///
    /// # Errors
    /// Fails if the text cannot be tokenized.
    pub fn encode_plain(&self, text: &str) -> Result<Vec<u32>> {
        self.inner
            .encode(text, false)
            .map(|e| e.get_ids().to_vec())
            .map_err(|e| EngineError::TokenizeFailed {
                detail: e.to_string(),
            })
    }

    /// # Errors
    /// Fails if the ids cannot be decoded.
    pub fn decode(&self, ids: &[u32], skip_special: bool) -> Result<String> {
        self.inner
            .decode(ids, skip_special)
            .map_err(|e| EngineError::TokenizeFailed {
                detail: e.to_string(),
            })
    }

    #[must_use]
    pub fn decoder(&self) -> IncrementalDecoder {
        IncrementalDecoder::default()
    }
}

fn meta_token_id(content: &gguf_file::Content, key: &str) -> Option<u32> {
    let v = content.metadata.get(key)?;
    v.to_u32()
        .ok()
        .or_else(|| v.to_u64().ok().and_then(|n| u32::try_from(n).ok()))
}

/// Turns a stream of token ids into a stream of complete text.
///
/// Master spec s8.6: emit only the newly completed suffix, and hold back while
/// the tail is an incomplete UTF-8 sequence. Byte-level BPE can split a
/// multi-byte character across two tokens, and decoding only the first half
/// yields U+FFFD -- which, once emitted, cannot be taken back out of the UI.
#[derive(Debug, Default)]
pub struct IncrementalDecoder {
    pending: Vec<u32>,
}

impl IncrementalDecoder {
    /// Feed one token; returns text ready to show, if any.
    ///
    /// # Errors
    /// Fails if the pending ids cannot be decoded at all.
    pub fn push(&mut self, tk: &EngineTokenizer, id: u32) -> Result<Option<String>> {
        self.pending.push(id);
        let text = tk.decode(&self.pending, true)?;

        // A trailing replacement character means the last token carried only
        // part of a character. Wait for the rest -- unless we have waited long
        // enough that it is clearly not coming.
        if text.ends_with('\u{FFFD}') && self.pending.len() < MAX_PENDING {
            return Ok(None);
        }
        self.pending.clear();
        Ok(if text.is_empty() { None } else { Some(text) })
    }

    /// Anything still held back when generation ends.
    ///
    /// # Errors
    /// Fails if the pending ids cannot be decoded.
    pub fn flush(&mut self, tk: &EngineTokenizer) -> Result<Option<String>> {
        if self.pending.is_empty() {
            return Ok(None);
        }
        let text = tk.decode(&self.pending, true)?;
        self.pending.clear();
        Ok(if text.is_empty() { None } else { Some(text) })
    }
}
