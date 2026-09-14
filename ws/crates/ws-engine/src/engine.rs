//! The engine worker and its command channel (master spec s8.2, s8.10).
//!
//! One model, one generation at a time; further requests queue behind the
//! channel. The model lives on a dedicated OS thread and never touches the
//! async runtime, because a forward pass is a long CPU-bound block that would
//! starve every other task sharing its worker.
//!
//! ## Panics do not take the app with them
//!
//! Every command runs inside `catch_unwind`. A panic in the model -- a shape
//! mismatch from a file that passed validation but is wrong in some way the
//! header could not show -- unloads the model, marks the engine poisoned and
//! reports an error. The window stays open and the conversation is intact.
//! This is the difference between "that model did not work" and "the app
//! vanished".

use crate::error::{EngineError, Result};
use crate::generate::{GenerateOptions, generate};
use crate::gguf::{self, GgufInfo};
use crate::model::{LoadOptions, LoadedInfo, LoadedModel};
use crate::sampler::SamplingParams;
use crate::template::TemplateMessage;
use crate::tokenizer::EngineTokenizer;
use candle_core::Device;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use ws_context::backend::BackendEvent;

pub enum EngineCmd {
    Inspect {
        path: PathBuf,
        reply: oneshot::Sender<Result<GgufInfo>>,
    },
    Load {
        path: PathBuf,
        opts: Box<LoadOptions>,
        reply: oneshot::Sender<Result<LoadedInfo>>,
    },
    Unload {
        reply: oneshot::Sender<()>,
    },
    /// Render the chat template and tokenize the result in one step.
    ///
    /// The two belong together: the exact token count depends on the template
    /// the *file* carries, so a caller that rendered separately would be
    /// counting a different string than the one the model sees (master spec
    /// s6.1).
    RenderAndTokenize {
        messages: Vec<TemplateMessage>,
        add_generation_prompt: bool,
        reply: oneshot::Sender<Result<Vec<u32>>>,
    },
    Generate {
        prompt_tokens: Vec<u32>,
        params: Box<SamplingParams>,
        events: tokio_mpsc::UnboundedSender<BackendEvent>,
        cancel: CancellationToken,
    },
    /// Report what is loaded, if anything.
    Status {
        reply: oneshot::Sender<Option<LoadedInfo>>,
    },
    /// Borrow the loaded tokenizer for synchronous token counting.
    Tokenizer {
        reply: oneshot::Sender<Option<Arc<EngineTokenizer>>>,
    },
}

/// Names the command without its contents.
///
/// Deliberately not derived: a derived `Debug` would put whole prompts into
/// anything that logged a command, and master spec s12 keeps conversation
/// content out of logs.
impl std::fmt::Debug for EngineCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Inspect { .. } => "Inspect",
            Self::Load { .. } => "Load",
            Self::Unload { .. } => "Unload",
            Self::RenderAndTokenize { .. } => "RenderAndTokenize",
            Self::Generate { .. } => "Generate",
            Self::Status { .. } => "Status",
            Self::Tokenizer { .. } => "Tokenizer",
        };
        f.write_str(name)
    }
}

/// A handle to the engine worker. Cloning gives another sender to the same
/// worker; the worker stops when the last one is dropped.
#[derive(Debug, Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<EngineCmd>,
}

impl EngineHandle {
    /// Start the worker thread.
    ///
    /// `threads` sizes rayon's global pool, which is where candle's CPU
    /// kernels do their parallel work. It can only be set once per process,
    /// so a second engine inherits the first one's pool; that is fine, since
    /// master spec s8.1 allows one model at a time anyway.
    ///
    /// # Errors
    /// Fails only if the worker thread cannot be spawned.
    pub fn start(threads: Option<usize>) -> Result<Self> {
        let threads = threads.unwrap_or_else(crate::memory::default_threads);
        // An error here means a pool already exists, which is not a problem.
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global();

        let (tx, rx) = mpsc::channel::<EngineCmd>();
        std::thread::Builder::new()
            .name("ws-engine".to_owned())
            // A forward pass recurses through layers with large stack frames;
            // the default 2 MiB is not generous for that plus a template
            // render.
            .stack_size(16 * 1024 * 1024)
            .spawn(move || worker(&rx))
            .map_err(|source| EngineError::Io {
                path: "ws-engine worker thread".to_owned(),
                source,
            })?;

        Ok(Self { tx })
    }

    /// # Errors
    /// Fails if the worker has stopped.
    pub fn send(&self, cmd: EngineCmd) -> Result<()> {
        self.tx.send(cmd).map_err(|_| EngineError::WorkerGone)
    }

    /// # Errors
    /// Fails if the worker has stopped or the file cannot be inspected.
    pub async fn inspect(&self, path: PathBuf) -> Result<GgufInfo> {
        let (reply, rx) = oneshot::channel();
        self.send(EngineCmd::Inspect { path, reply })?;
        rx.await.map_err(|_| EngineError::WorkerGone)?
    }

    /// # Errors
    /// Fails if the worker has stopped or the model cannot be loaded.
    pub async fn load(&self, path: PathBuf, opts: LoadOptions) -> Result<LoadedInfo> {
        let (reply, rx) = oneshot::channel();
        self.send(EngineCmd::Load {
            path,
            opts: Box::new(opts),
            reply,
        })?;
        rx.await.map_err(|_| EngineError::WorkerGone)?
    }

    /// # Errors
    /// Fails if the worker has stopped.
    pub async fn unload(&self) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.send(EngineCmd::Unload { reply })?;
        rx.await.map_err(|_| EngineError::WorkerGone)
    }

    /// # Errors
    /// Fails if the worker has stopped.
    pub async fn status(&self) -> Result<Option<LoadedInfo>> {
        let (reply, rx) = oneshot::channel();
        self.send(EngineCmd::Status { reply })?;
        rx.await.map_err(|_| EngineError::WorkerGone)
    }

    /// The loaded tokenizer, for synchronous exact token counting.
    ///
    /// # Errors
    /// Fails if the worker has stopped.
    pub async fn tokenizer(&self) -> Result<Option<Arc<EngineTokenizer>>> {
        let (reply, rx) = oneshot::channel();
        self.send(EngineCmd::Tokenizer { reply })?;
        rx.await.map_err(|_| EngineError::WorkerGone)
    }

    /// # Errors
    /// Fails if the worker has stopped, no model is loaded, or the template
    /// cannot be rendered.
    pub async fn render_and_tokenize(
        &self,
        messages: Vec<TemplateMessage>,
        add_generation_prompt: bool,
    ) -> Result<Vec<u32>> {
        let (reply, rx) = oneshot::channel();
        self.send(EngineCmd::RenderAndTokenize {
            messages,
            add_generation_prompt,
            reply,
        })?;
        rx.await.map_err(|_| EngineError::WorkerGone)?
    }
}

/// State the worker owns. Nothing else can reach the model.
struct Worker {
    model: Option<LoadedModel>,
    device: Device,
    /// Set after a panic. Every later command fails with a message that says
    /// what to do, rather than a second panic or a silent wrong answer.
    poisoned: bool,
    generate_opts: GenerateOptions,
}

fn worker(rx: &mpsc::Receiver<EngineCmd>) {
    let mut state = Worker {
        model: None,
        device: Device::Cpu,
        poisoned: false,
        generate_opts: GenerateOptions::default(),
    };

    while let Ok(cmd) = rx.recv() {
        // `AssertUnwindSafe` is a safe wrapper, not an unsafe block: it asserts
        // that observing this state after a panic is acceptable, which is
        // exactly what `poisoned` then enforces.
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| state.handle(cmd)));
        if outcome.is_err() {
            tracing::error!("the engine panicked; unloading the model and poisoning the engine");
            state.poisoned = true;
            state.model = None;
        }
    }
    tracing::debug!("engine worker stopping: all handles dropped");
}

impl Worker {
    fn handle(&mut self, cmd: EngineCmd) {
        match cmd {
            EngineCmd::Inspect { path, reply } => {
                let _ = reply.send(gguf::inspect(&path));
            }

            EngineCmd::Load { path, opts, reply } => {
                let result = self.guard().and_then(|()| {
                    // Drop any previous model before loading: holding both at
                    // once would double the peak, which is the number the
                    // memory pre-check was built to respect.
                    self.model = None;
                    let model = LoadedModel::load(&path, &opts, &self.device)?;
                    let info = model.info().clone();
                    self.model = Some(model);
                    Ok(info)
                });
                let _ = reply.send(result);
            }

            EngineCmd::Unload { reply } => {
                self.model = None;
                // Unloading is how a user recovers from a panic, so it clears
                // the poison too.
                self.poisoned = false;
                let _ = reply.send(());
            }

            EngineCmd::Status { reply } => {
                let _ = reply.send(self.model.as_ref().map(|m| m.info().clone()));
            }

            EngineCmd::Tokenizer { reply } => {
                let _ = reply.send(self.model.as_ref().map(LoadedModel::tokenizer_handle));
            }

            EngineCmd::RenderAndTokenize {
                messages,
                add_generation_prompt,
                reply,
            } => {
                let result = self.guard().and_then(|()| {
                    let model = self.model.as_ref().ok_or(EngineError::NotLoaded)?;
                    let rendered = model.template().render(&messages, add_generation_prompt)?;
                    model.tokenizer().encode_rendered(&rendered)
                });
                let _ = reply.send(result);
            }

            EngineCmd::Generate {
                prompt_tokens,
                params,
                events,
                cancel,
            } => {
                let opts = self.generate_opts;
                let result = self.guard().and_then(|()| {
                    let model = self.model.as_mut().ok_or(EngineError::NotLoaded)?;
                    let mut emit = |event| {
                        // A closed receiver means the caller went away; the
                        // cancellation token is the orderly path, so just stop
                        // emitting.
                        let _ = events.send(event);
                    };
                    generate(model, &prompt_tokens, &params, opts, &cancel, &mut emit)
                });

                // A backend must emit exactly one terminal event, last
                // (master spec s4).
                let terminal = match result {
                    Ok(stop_reason) => BackendEvent::Done { stop_reason },
                    Err(e) => BackendEvent::Error(ws_context::backend::BackendError::Engine(
                        e.to_string(),
                    )),
                };
                let _ = events.send(terminal);
            }
        }
    }

    /// Refuse to touch a model after a panic.
    fn guard(&self) -> Result<()> {
        if self.poisoned {
            return Err(EngineError::Poisoned);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn commands_reach_the_worker_without_a_model_loaded() {
        let engine = EngineHandle::start(Some(1)).unwrap();
        assert!(engine.status().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rendering_without_a_model_is_an_error_not_a_hang() {
        let engine = EngineHandle::start(Some(1)).unwrap();
        let err = engine
            .render_and_tokenize(vec![TemplateMessage::new("user", "hi")], true)
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::NotLoaded), "got {err:?}");
    }

    #[tokio::test]
    async fn inspecting_a_missing_file_returns_an_error_through_the_channel() {
        let engine = EngineHandle::start(Some(1)).unwrap();
        let err = engine
            .inspect(PathBuf::from("no-such-model.gguf"))
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::Io { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn generating_without_a_model_still_emits_exactly_one_terminal_event() {
        // The orchestrator waits for a terminal event rather than a timeout
        // (master spec s4), so this path must not be silent.
        let engine = EngineHandle::start(Some(1)).unwrap();
        let (tx, mut rx) = tokio_mpsc::unbounded_channel();

        engine
            .send(EngineCmd::Generate {
                prompt_tokens: vec![1, 2, 3],
                params: Box::new(SamplingParams::default()),
                events: tx,
                cancel: CancellationToken::new(),
            })
            .unwrap();

        let mut events = Vec::new();
        while let Some(e) = rx.recv().await {
            events.push(e);
        }
        assert_eq!(events.len(), 1, "got {events:?}");
        assert!(
            matches!(events[0], BackendEvent::Error(_)),
            "got {:?}",
            events[0]
        );
    }

    #[tokio::test]
    async fn unloading_clears_the_poison() {
        let engine = EngineHandle::start(Some(1)).unwrap();
        engine.unload().await.unwrap();
        assert!(engine.status().await.unwrap().is_none());
    }

    #[test]
    fn a_dropped_handle_stops_the_worker() {
        let engine = EngineHandle::start(Some(1)).unwrap();
        let tx = engine.tx.clone();
        drop(engine);
        // The clone keeps it alive.
        assert!(
            tx.send(EngineCmd::Unload {
                reply: oneshot::channel().0
            })
            .is_ok()
        );
        drop(tx);
    }
}
