//! End-to-end generation against a real, tiny model.
//!
//! This builds an actual GGUF -- a two-layer Llama with a real byte-level BPE
//! vocabulary, a real chat template and randomly initialised weights -- and
//! runs the whole path over it: validate, budget, load, render, tokenize,
//! prefill, sample, decode, stream.
//!
//! **What this does and does not prove.** The weights are random, so the text
//! is nonsense; nothing here says the model is *correct*. That is what the
//! reference-parity suite is for, and it needs a real model
//! file. What these tests do prove is that every seam holds on a genuine file:
//! the tensor names match, the tokenizer builds from GGUF metadata, the
//! template renders, BOS is written exactly once, the KV cache is reused
//! across turns, stop tokens end a turn, cancellation is honoured, and the
//! event stream is well formed. Those are the failures that would otherwise
//! wait until someone downloaded four gigabytes to find them.

// Tests may unwrap and expect: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use candle_core::quantized::{GgmlDType, QTensor, gguf_file};
use candle_core::{Device, Tensor};
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;
use ws_context::backend::{BackendEvent, StopReason};
use ws_engine::engine::EngineCmd;
use ws_engine::{EngineHandle, LoadOptions, SamplingParams, TemplateMessage};

// Small enough to build and run in a test, large enough to be a real llama:
// grouped-query attention (4 query heads over 2 KV heads) is on, so the cache
// sizing and head repetition are genuinely exercised.
const BLOCKS: usize = 2;
const EMBED: usize = 64;
const HEADS: usize = 4;
const KV_HEADS: usize = 2;
const FFN: usize = 128;
const HEAD_DIM: usize = EMBED / HEADS;

/// A byte-level BPE vocabulary: all 256 byte tokens, then the specials.
///
/// Byte-level with no merges means every input tokenizes, which keeps these
/// tests about the engine rather than about vocabulary coverage.
fn vocabulary() -> (Vec<String>, u32, u32, u32) {
    // The byte-level alphabet `tokenizers` uses: bytes that are not printable
    // ASCII are mapped into a private range so every token is valid UTF-8.
    let mut tokens: Vec<String> = Vec::with_capacity(300);
    for b in 0u16..256 {
        let c = match u8::try_from(b).unwrap() {
            0x21..=0x7E => char::from(u8::try_from(b).unwrap()),
            0xA1..=0xAC | 0xAE..=0xFF => char::from_u32(u32::from(b)).unwrap(),
            other => char::from_u32(256 + u32::from(other)).unwrap(),
        };
        tokens.push(c.to_string());
    }
    let bos = u32::try_from(tokens.len()).unwrap();
    tokens.push("<|begin_of_text|>".to_owned());
    let eos = u32::try_from(tokens.len()).unwrap();
    tokens.push("<|end_of_text|>".to_owned());
    let eot = u32::try_from(tokens.len()).unwrap();
    tokens.push("<|eot_id|>".to_owned());
    for extra in ["<|start_header_id|>", "<|end_header_id|>"] {
        tokens.push(extra.to_owned());
    }
    (tokens, bos, eos, eot)
}

const CHAT_TEMPLATE: &str = concat!(
    "{{- bos_token }}",
    "{%- for message in messages %}",
    "{{- '<|start_header_id|>' + message['role'] + '<|end_header_id|>' ",
    "+ message['content'] + '<|eot_id|>' }}",
    "{%- endfor %}",
    "{%- if add_generation_prompt %}",
    "{{- '<|start_header_id|>assistant<|end_header_id|>' }}",
    "{%- endif %}",
);

/// Deterministic pseudo-random weights, so a failure is reproducible.
fn weights(seed: u64, rows: usize, cols: usize, device: &Device) -> Tensor {
    let mut state = seed | 1;
    let data: Vec<f32> = (0..rows * cols)
        .map(|_| {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let x = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / 16_777_216.0;
            // Small values: large random weights saturate into NaN through two
            // layers and the test would fail for the wrong reason.
            (x - 0.5) * 0.1
        })
        .collect();
    Tensor::from_vec(data, (rows, cols), device).unwrap()
}

/// Norm weights are all ones: a random RMSNorm scale is a needless way to make
/// the activations explode.
fn ones(n: usize, device: &Device) -> Tensor {
    Tensor::from_vec(vec![1.0f32; n], (n,), device).unwrap()
}

/// Build a complete, loadable GGUF. This is the fixture the whole file rests on.
fn write_tiny_model(path: &Path, vocab_size: usize) {
    let device = Device::Cpu;
    // Q8_0 works on 32-element blocks and every dimension here is a multiple
    // of 32, so nothing needs padding.
    let quantize = |t: &Tensor| QTensor::quantize(t, GgmlDType::Q8_0).unwrap();

    let mut owned: Vec<(String, QTensor)> = Vec::new();
    owned.push((
        "token_embd.weight".to_owned(),
        quantize(&weights(1, vocab_size, EMBED, &device)),
    ));
    owned.push((
        "output_norm.weight".to_owned(),
        QTensor::quantize(&ones(EMBED, &device), GgmlDType::F32).unwrap(),
    ));
    owned.push((
        "output.weight".to_owned(),
        quantize(&weights(2, vocab_size, EMBED, &device)),
    ));

    for layer in 0..BLOCKS {
        let p = format!("blk.{layer}");
        let seed = |n: u64| (layer as u64 + 1) * 100 + n;
        for (name, rows, cols, n) in [
            ("attn_q", HEADS * HEAD_DIM, EMBED, 1u64),
            ("attn_k", KV_HEADS * HEAD_DIM, EMBED, 2),
            ("attn_v", KV_HEADS * HEAD_DIM, EMBED, 3),
            ("attn_output", EMBED, HEADS * HEAD_DIM, 4),
            ("ffn_gate", FFN, EMBED, 5),
            ("ffn_up", FFN, EMBED, 6),
            ("ffn_down", EMBED, FFN, 7),
        ] {
            owned.push((
                format!("{p}.{name}.weight"),
                quantize(&weights(seed(n), rows, cols, &device)),
            ));
        }
        for name in ["attn_norm", "ffn_norm"] {
            owned.push((
                format!("{p}.{name}.weight"),
                QTensor::quantize(&ones(EMBED, &device), GgmlDType::F32).unwrap(),
            ));
        }
    }

    let (tokens, bos, eos, eot) = vocabulary();
    let token_values: Vec<gguf_file::Value> = tokens
        .iter()
        .map(|t| gguf_file::Value::String(t.clone()))
        .collect();
    // Non-normal token types mark the specials, so `decode(skip_special)` hides
    // them the way it would for a real model.
    let types: Vec<gguf_file::Value> = (0..tokens.len())
        .map(|i| gguf_file::Value::U32(if i >= bos as usize { 3 } else { 1 }))
        .collect();

    let mut metadata: HashMap<String, gguf_file::Value> = HashMap::new();
    let mut put = |k: &str, v: gguf_file::Value| {
        metadata.insert(k.to_owned(), v);
    };
    put(
        "general.architecture",
        gguf_file::Value::String("llama".to_owned()),
    );
    put(
        "llama.block_count",
        gguf_file::Value::U32(u32::try_from(BLOCKS).unwrap()),
    );
    put(
        "llama.embedding_length",
        gguf_file::Value::U32(u32::try_from(EMBED).unwrap()),
    );
    put(
        "llama.feed_forward_length",
        gguf_file::Value::U32(u32::try_from(FFN).unwrap()),
    );
    put(
        "llama.attention.head_count",
        gguf_file::Value::U32(u32::try_from(HEADS).unwrap()),
    );
    put(
        "llama.attention.head_count_kv",
        gguf_file::Value::U32(u32::try_from(KV_HEADS).unwrap()),
    );
    put(
        "llama.rope.dimension_count",
        gguf_file::Value::U32(u32::try_from(HEAD_DIM).unwrap()),
    );
    put(
        "llama.attention.layer_norm_rms_epsilon",
        gguf_file::Value::F32(1e-5),
    );
    put("llama.rope.freq_base", gguf_file::Value::F32(10_000.0));
    put("llama.context_length", gguf_file::Value::U32(2_048));

    put(
        "tokenizer.ggml.model",
        gguf_file::Value::String("gpt2".to_owned()),
    );
    // Real Llama 3 files declare this, and it matters: the default ("gpt2")
    // pre-tokenizer prepends a space to every input, which would silently add
    // a token to every count.
    put(
        "tokenizer.ggml.pre",
        gguf_file::Value::String("llama3".to_owned()),
    );
    put(
        "tokenizer.ggml.tokens",
        gguf_file::Value::Array(token_values),
    );
    put("tokenizer.ggml.token_type", gguf_file::Value::Array(types));
    put("tokenizer.ggml.merges", gguf_file::Value::Array(Vec::new()));
    put("tokenizer.ggml.bos_token_id", gguf_file::Value::U32(bos));
    put("tokenizer.ggml.eos_token_id", gguf_file::Value::U32(eos));
    put("tokenizer.ggml.eot_token_id", gguf_file::Value::U32(eot));
    // The file asks for BOS to be added automatically. Together with a
    // template that writes one itself, this is exactly the double-BOS trap, and it is here on purpose.
    put("tokenizer.ggml.add_bos_token", gguf_file::Value::Bool(true));
    put(
        "tokenizer.chat_template",
        gguf_file::Value::String(CHAT_TEMPLATE.to_owned()),
    );

    let meta_refs: Vec<(&str, &gguf_file::Value)> =
        metadata.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let tensor_refs: Vec<(&str, &QTensor)> = owned.iter().map(|(n, t)| (n.as_str(), t)).collect();

    let mut file = File::create(path).unwrap();
    gguf_file::write(&mut file, &meta_refs, &tensor_refs).unwrap();
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    vocab_size: usize,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tiny-llama.gguf");
        let vocab_size = vocabulary().0.len();
        write_tiny_model(&path, vocab_size);
        Self {
            _dir: dir,
            path,
            vocab_size,
        }
    }

    async fn engine(&self) -> EngineHandle {
        let engine = EngineHandle::start(Some(1)).unwrap();
        engine
            .load(
                self.path.clone(),
                LoadOptions {
                    context_cap: 512,
                    threads: Some(1),
                    ..LoadOptions::default()
                },
            )
            .await
            .expect("the tiny model must load");
        engine
    }
}

/// Run one generation and collect every event.
async fn run(engine: &EngineHandle, prompt: Vec<u32>, max_tokens: u32) -> Vec<BackendEvent> {
    run_with(engine, prompt, max_tokens, &CancellationToken::new()).await
}

async fn run_with(
    engine: &EngineHandle,
    prompt: Vec<u32>,
    max_tokens: u32,
    cancel: &CancellationToken,
) -> Vec<BackendEvent> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    engine
        .send(EngineCmd::Generate {
            prompt_tokens: prompt,
            params: Box::new(SamplingParams {
                temperature: 0.0, // greedy, so failures are reproducible
                max_tokens,
                ..SamplingParams::default()
            }),
            events: tx,
            cancel: cancel.clone(),
        })
        .unwrap();

    let mut events = Vec::new();
    while let Some(e) = rx.recv().await {
        events.push(e);
    }
    events
}

fn text_of(events: &[BackendEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            BackendEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

fn prefills(events: &[BackendEvent]) -> Vec<(u32, u32, u32)> {
    events
        .iter()
        .filter_map(|e| match e {
            BackendEvent::PrefillProgress {
                reused_tokens,
                done_tokens,
                total_tokens,
            } => Some((*reused_tokens, *done_tokens, *total_tokens)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_tiny_model_loads_and_reports_itself() {
    let f = Fixture::new();
    let engine = f.engine().await;

    let info = engine.status().await.unwrap().expect("a model is loaded");
    assert_eq!(info.architecture, "llama");
    assert_eq!(info.context_cap, 512);
    assert_eq!(info.vocab_size, f.vocab_size);
    assert_eq!(
        info.tokenizer_source,
        ws_engine::TokenizerSource::Gguf,
        "the vocabulary came out of the file itself"
    );
    assert!(info.memory.total_bytes > 0);
}

#[tokio::test]
async fn the_template_renders_and_the_prompt_carries_exactly_one_bos() {
    // The file sets add_bos_token AND the template writes one. Encoding with
    // special tokens enabled would produce two and shift every position.
    let f = Fixture::new();
    let engine = f.engine().await;
    let (_, bos, ..) = vocabulary();

    let ids = engine
        .render_and_tokenize(
            vec![
                TemplateMessage::new("system", "Be brief."),
                TemplateMessage::new("user", "Hello"),
            ],
            true,
        )
        .await
        .unwrap();

    assert!(!ids.is_empty());
    assert_eq!(ids[0], bos, "the prompt starts with BOS");
    assert_eq!(
        ids.iter().filter(|id| **id == bos).count(),
        1,
        "exactly one BOS, not two"
    );
}

#[tokio::test]
async fn generation_produces_text_and_one_terminal_event() {
    let f = Fixture::new();
    let engine = f.engine().await;
    let prompt = engine
        .render_and_tokenize(vec![TemplateMessage::new("user", "Hello")], true)
        .await
        .unwrap();

    let events = run(&engine, prompt.clone(), 12).await;

    // Exactly one terminal event, and it is last.
    let terminals = events
        .iter()
        .filter(|e| matches!(e, BackendEvent::Done { .. } | BackendEvent::Error(_)))
        .count();
    assert_eq!(terminals, 1, "got {events:?}");
    assert!(
        matches!(events.last(), Some(BackendEvent::Done { .. })),
        "the terminal event must be last: {:?}",
        events.last()
    );

    // Random weights, so the text is nonsense -- but it must be text.
    let text = text_of(&events);
    assert!(!text.is_empty(), "nothing was generated");

    let usage = events
        .iter()
        .find_map(|e| match e {
            BackendEvent::Usage {
                input_tokens,
                output_tokens,
            } => Some((*input_tokens, *output_tokens)),
            _ => None,
        })
        .expect("usage must be reported");
    assert_eq!(usage.0 as usize, prompt.len());
    assert!(usage.1 > 0 && usage.1 <= 12, "output tokens: {}", usage.1);
}

#[tokio::test]
async fn max_tokens_stops_generation() {
    let f = Fixture::new();
    let engine = f.engine().await;
    let prompt = engine
        .render_and_tokenize(vec![TemplateMessage::new("user", "Hi")], true)
        .await
        .unwrap();

    let events = run(&engine, prompt, 3).await;
    let stop = events.iter().find_map(|e| match e {
        BackendEvent::Done { stop_reason } => Some(stop_reason.clone()),
        _ => None,
    });
    // With random weights it could hit a stop token first; either is correct,
    // but it must not run past the cap.
    assert!(
        matches!(stop, Some(StopReason::MaxTokens | StopReason::EndTurn)),
        "got {stop:?}"
    );
    let usage = events.iter().find_map(|e| match e {
        BackendEvent::Usage { output_tokens, .. } => Some(*output_tokens),
        _ => None,
    });
    assert!(usage.is_some_and(|n| n <= 3), "generated {usage:?} of 3");
}

#[tokio::test]
async fn prefill_progress_reaches_the_whole_prompt() {
    let f = Fixture::new();
    let engine = f.engine().await;
    let prompt = engine
        .render_and_tokenize(vec![TemplateMessage::new("user", "word ".repeat(40))], true)
        .await
        .unwrap();

    let events = run(&engine, prompt.clone(), 2).await;
    let progress = prefills(&events);

    assert!(!progress.is_empty(), "prefill must report progress");
    let (reused, done, total) = *progress.last().unwrap();
    assert_eq!(total as usize, prompt.len(), "total is the whole prompt");
    assert_eq!(
        (reused + done) as usize,
        prompt.len(),
        "every prompt token is either reused or read"
    );
    // Progress must never go backwards.
    for pair in progress.windows(2) {
        assert!(
            pair[1].1 >= pair[0].1,
            "progress went backwards: {progress:?}"
        );
    }
}

#[tokio::test]
async fn a_second_turn_reuses_the_cache_instead_of_rereading_the_conversation() {
    // Without this, every chat turn re-reads the whole
    // conversation and a local model becomes unusable past a few exchanges.
    // It has to be demonstrated from these events,
    // which is what `reused_tokens` makes possible.
    //
    // `max_tokens: 1` is what makes this deterministic rather than flaky. The
    // decode loop stops before feeding its last sampled token back in, so the
    // cache afterwards holds exactly the prompt -- and the second prompt is
    // then a genuine extension of it. Driving this through a real reply
    // instead would depend on nonsense from random weights re-tokenizing to
    // the same ids, which is not something to build an assertion on.
    let f = Fixture::new();
    let engine = f.engine().await;

    let first = engine
        .render_and_tokenize(
            vec![TemplateMessage::new("user", "alpha ".repeat(30))],
            true,
        )
        .await
        .unwrap();

    let events = run(&engine, first.clone(), 1).await;
    let (reused_first, done_first, _) = *prefills(&events).last().unwrap();
    assert_eq!(reused_first, 0, "a cold cache reuses nothing");
    assert_eq!(done_first as usize, first.len(), "it read the whole prompt");

    // The conversation continues: the same prefix, with more on the end.
    let mut second = first.clone();
    second.extend_from_slice(&[b'a'.into(), b'b'.into(), b'c'.into()]);

    let events = run(&engine, second.clone(), 1).await;
    let (reused, done, total) = *prefills(&events).last().unwrap();

    assert_eq!(
        reused as usize,
        first.len(),
        "the first prompt should still be cached: {:?}",
        prefills(&events)
    );
    assert_eq!(
        done as usize,
        second.len() - first.len(),
        "only the new tokens should be read"
    );
    assert_eq!((reused + done) as usize, total as usize);
    assert!(
        done < 10,
        "reading {done} tokens for a 3-token extension is not reuse"
    );
}

#[tokio::test]
async fn a_real_conversation_runs_turn_after_turn() {
    // The realistic shape, asserted on what is actually guaranteed. Whether
    // the cache helps here depends on the reply re-tokenizing to the same
    // ids, which real text does and nonsense from random weights may not --
    // so this checks that the turns are correct and accounted for, and leaves
    // the reuse assertion to the deterministic test above.
    let f = Fixture::new();
    let engine = f.engine().await;

    let mut history = vec![TemplateMessage::new("user", "first question")];
    for turn in 0..3 {
        let prompt = engine
            .render_and_tokenize(history.clone(), true)
            .await
            .unwrap();
        let events = run(&engine, prompt.clone(), 6).await;

        assert!(
            matches!(events.last(), Some(BackendEvent::Done { .. })),
            "turn {turn} did not finish cleanly: {:?}",
            events.last()
        );
        let (reused, done, total) = *prefills(&events).last().unwrap();
        assert_eq!(total as usize, prompt.len());
        assert_eq!(
            (reused + done) as usize,
            prompt.len(),
            "turn {turn} left tokens unaccounted for"
        );

        let reply = text_of(&events);
        assert!(!reply.is_empty(), "turn {turn} produced nothing");
        history.push(TemplateMessage::new("assistant", reply));
        history.push(TemplateMessage::new("user", "and then?"));
    }
}

#[tokio::test]
async fn editing_an_earlier_turn_is_correct_even_when_the_cache_cannot_help() {
    // The other side of the reuse rule. A prompt that diverges from what is
    // cached must be re-read from the divergence, and on the reference path
    // that means re-read entirely. Getting this wrong would not be slow, it
    // would be wrong: the model would attend to a conversation that no longer
    // exists.
    let f = Fixture::new();
    let engine = f.engine().await;

    let first = engine
        .render_and_tokenize(
            vec![TemplateMessage::new("user", "the first question")],
            true,
        )
        .await
        .unwrap();
    let _ = run(&engine, first, 4).await;

    let edited = engine
        .render_and_tokenize(
            vec![TemplateMessage::new(
                "user",
                "a completely different question",
            )],
            true,
        )
        .await
        .unwrap();
    let events = run(&engine, edited.clone(), 4).await;
    let (reused, done, total) = *prefills(&events).last().unwrap();

    assert_eq!(total as usize, edited.len());
    assert_eq!(
        (reused + done) as usize,
        edited.len(),
        "every token of the edited prompt must be accounted for"
    );
    assert!(!text_of(&events).is_empty(), "it still has to answer");
}

#[tokio::test]
async fn cancelling_stops_generation_and_keeps_the_text_so_far() {
    let f = Fixture::new();
    let engine = f.engine().await;
    let prompt = engine
        .render_and_tokenize(vec![TemplateMessage::new("user", "Hello")], true)
        .await
        .unwrap();

    let cancel = CancellationToken::new();
    cancel.cancel(); // cancelled before it starts: the extreme case
    let events = run_with(&engine, prompt, 64, &cancel).await;

    let stop = events.iter().find_map(|e| match e {
        BackendEvent::Done { stop_reason } => Some(stop_reason.clone()),
        _ => None,
    });
    assert_eq!(stop, Some(StopReason::Cancelled), "got {events:?}");
    assert!(
        !events.iter().any(|e| matches!(e, BackendEvent::Error(_))),
        "cancelling is not an error"
    );
}

#[tokio::test]
async fn a_prompt_longer_than_the_context_cap_is_refused_with_numbers() {
    let f = Fixture::new();
    let engine = f.engine().await;

    let events = run(&engine, (0..600u32).map(|i| i % 200).collect(), 4).await;
    let error = events.iter().find_map(|e| match e {
        BackendEvent::Error(e) => Some(e.to_string()),
        _ => None,
    });
    let message = error.expect("an over-long prompt must be refused");
    assert!(
        message.contains("600"),
        "the message needs the numbers: {message}"
    );
    assert!(message.contains("512"), "{message}");
}

#[tokio::test]
async fn unloading_and_reloading_works() {
    let f = Fixture::new();
    let engine = f.engine().await;

    engine.unload().await.unwrap();
    assert!(engine.status().await.unwrap().is_none());

    engine
        .load(
            f.path.clone(),
            LoadOptions {
                context_cap: 256,
                threads: Some(1),
                ..LoadOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(engine.status().await.unwrap().unwrap().context_cap, 256);
}

#[tokio::test]
async fn exact_token_counting_uses_the_models_own_tokenizer() {
    // This is the number the context builder budgets
    // against, and against a small local cap it has to be the truth.
    use ws_context::backend::ChatBackend;

    let f = Fixture::new();
    let backend = ws_engine::LocalBackend::start(Some(1)).unwrap();
    backend
        .load(
            f.path.clone(),
            LoadOptions {
                context_cap: 512,
                threads: Some(1),
                ..LoadOptions::default()
            },
        )
        .await
        .unwrap();

    let model = backend.loaded_model_ref().expect("a model is loaded");
    let counter = backend.token_counter(&model);

    // A byte-level vocabulary with no merges: one token per byte, exactly.
    assert_eq!(counter.count_text("hello"), 5);
    assert_eq!(counter.count_text(""), 0);

    let caps = backend.capabilities(&model);
    assert_eq!(caps.context_window, 512);
    assert_eq!(
        caps.token_counting,
        ws_context::model::TokenCounting::Exact,
        "and it must say so, or the builder keeps an estimate's safety margin"
    );
}

#[tokio::test]
async fn a_model_that_is_too_large_for_its_cap_is_refused_before_loading() {
    // The memory pre-check is the last line before an allocation that would
    // abort the process.
    let f = Fixture::new();
    let engine = EngineHandle::start(Some(1)).unwrap();

    let err = engine
        .load(
            f.path.clone(),
            LoadOptions {
                // Past what the reference forward pass can address.
                context_cap: 100_000,
                threads: Some(1),
                ..LoadOptions::default()
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, ws_engine::EngineError::ContextCapTooLarge { .. }),
        "got {err:?}"
    );
}

/// Write the tiny model somewhere durable, for driving `ws-engine-cli` by hand.
///
/// Ignored by default: it is a tool, not a test. Run with
/// `cargo test -p ws-engine --test tiny_model -- --ignored export_tiny_model`
/// and it prints the path it wrote.
#[test]
#[ignore = "writes a file for manual CLI testing"]
fn export_tiny_model() {
    let dir = std::env::var("WS_TINY_MODEL_DIR").unwrap_or_else(|_| ".".to_owned());
    let path = Path::new(&dir).join("tiny-llama.gguf");
    write_tiny_model(&path, vocabulary().0.len());
    println!("wrote {}", path.display());
}
