//! Shared streaming machinery for the API adapters.
//!
//! Every adapter reads an SSE body, decodes it, and must handle two hazards
//! identically: a multi-byte UTF-8 sequence straddling a chunk boundary, and
//! cancellation arriving while the provider is quiet. Those live here so there
//! is one implementation to get right rather than one per provider.

use bytes::Bytes;
use futures_util::stream::{BoxStream, StreamExt};
use std::collections::VecDeque;
use tokio_util::sync::CancellationToken;
use ws_context::{BackendError, BackendEvent, BackendId, StopReason};

use crate::sse::{SseEvent, SseParser};

/// A provider's stream decoder. Implementations are pure state machines, so
/// the whole protocol is testable from recorded fixtures.
pub trait StreamDecoder: Send + 'static {
    /// Decode one SSE event into zero or more backend events.
    fn on_event(&mut self, ev: &SseEvent) -> Vec<BackendEvent>;

    /// Called when the body ends. Should report a protocol error unless the
    /// stream already closed cleanly.
    fn finish(&mut self) -> Vec<BackendEvent>;
}

struct State<D: StreamDecoder> {
    bytes: BoxStream<'static, reqwest::Result<Bytes>>,
    parser: SseParser,
    decoder: D,
    queue: VecDeque<BackendEvent>,
    cancel: CancellationToken,
    backend: BackendId,
    finished: bool,
    /// Bytes of an incomplete UTF-8 sequence left by the previous chunk.
    pending_tail: Vec<u8>,
}

impl<D: StreamDecoder> State<D> {
    fn absorb(&mut self, chunk: &[u8]) {
        // A multi-byte UTF-8 sequence can straddle a chunk boundary, and
        // from_utf8_lossy would turn the split halves into replacement
        // characters -- silently corrupting any non-ASCII reply. Decode the
        // valid prefix and carry the remainder into the next chunk.
        let mut buf = std::mem::take(&mut self.pending_tail);
        buf.extend_from_slice(chunk);

        let (text, tail) = match std::str::from_utf8(&buf) {
            Ok(text) => (text.to_owned(), Vec::new()),
            Err(e) => {
                let valid = e.valid_up_to();
                let head = String::from_utf8_lossy(&buf[..valid]).into_owned();
                match e.error_len() {
                    // Genuinely invalid bytes: skip them rather than waiting
                    // forever for a continuation that will never come.
                    Some(bad) => (head, buf[valid + bad..].to_vec()),
                    // Incomplete: keep it for the next chunk.
                    None => (head, buf[valid..].to_vec()),
                }
            }
        };
        self.pending_tail = tail;

        for ev in self.parser.push(&text) {
            let decoded = self.decoder.on_event(&ev);
            self.queue.extend(decoded);
        }
    }
}

/// Turn an HTTP response body into an ordered stream of backend events.
pub fn event_stream<D: StreamDecoder>(
    response: reqwest::Response,
    decoder: D,
    backend: BackendId,
    cancel: CancellationToken,
) -> BoxStream<'static, BackendEvent> {
    let state = State {
        bytes: response.bytes_stream().boxed(),
        parser: SseParser::new(),
        decoder,
        queue: VecDeque::new(),
        cancel,
        backend,
        finished: false,
        pending_tail: Vec::new(),
    };

    futures_util::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(ev) = st.queue.pop_front() {
                return Some((ev, st));
            }
            if st.finished {
                return None;
            }
            if st.cancel.is_cancelled() {
                st.finished = true;
                st.queue.push_back(BackendEvent::Done {
                    stop_reason: StopReason::Cancelled,
                });
                continue;
            }

            // Race the next chunk against cancellation, so stop stays
            // responsive when the provider has gone quiet.
            let next = tokio::select! {
                biased;
                () = st.cancel.cancelled() => {
                    st.finished = true;
                    st.queue.push_back(BackendEvent::Done {
                        stop_reason: StopReason::Cancelled,
                    });
                    continue;
                }
                chunk = st.bytes.next() => chunk,
            };

            match next {
                Some(Ok(chunk)) => st.absorb(&chunk),
                Some(Err(e)) => {
                    st.finished = true;
                    st.queue
                        .push_back(BackendEvent::Error(BackendError::Transport {
                            backend: st.backend.clone(),
                            message: e.to_string(),
                        }));
                }
                None => {
                    st.finished = true;
                    for ev in st.parser.finish() {
                        let decoded = st.decoder.on_event(&ev);
                        st.queue.extend(decoded);
                    }
                    let tail = st.decoder.finish();
                    st.queue.extend(tail);
                }
            }
        }
    })
    .boxed()
}

/// Decode a whole SSE body with `decoder`, feeding it in `chunk_size`-character
/// pieces. Exists for fixture tests, which need the same path as the live
/// stream without a network.
pub fn decode_body<D: StreamDecoder>(
    body: &str,
    decoder: &mut D,
    chunk_size: usize,
) -> Vec<BackendEvent> {
    let mut parser = SseParser::new();
    let mut events = Vec::new();
    let chars: Vec<char> = body.chars().collect();

    for piece in chars.chunks(chunk_size.max(1)) {
        let s: String = piece.iter().collect();
        for ev in parser.push(&s) {
            events.extend(decoder.on_event(&ev));
        }
    }
    for ev in parser.finish() {
        events.extend(decoder.on_event(&ev));
    }
    events
}

/// Zero or more events plus the SSE event that produced them, for tests that
/// need to assert ordering across a whole fixture.
pub type Decoded = Vec<(SseEvent, Vec<BackendEvent>)>;
