//! A Server-Sent Events parser (master spec s7.1).
//!
//! Written in-house because the job is small and the failure mode is subtle:
//! HTTP chunks do not align with lines, so a naive parser that treats each
//! chunk as a batch of complete lines will corrupt any event that straddles a
//! boundary. That shows up as rare, unreproducible truncation under load,
//! which is exactly the kind of bug that is painful to find later.
//!
//! Follows the WHATWG event-stream rules that matter here: `\r\n`, `\n` and
//! `\r` all terminate a line, a leading colon marks a comment, one optional
//! space after the field colon is stripped, `data` fields accumulate joined by
//! newlines, and a blank line dispatches the event.

/// A dispatched event: its `event:` type, if any, and its accumulated data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental parser. Feed it chunks in arrival order.
#[derive(Debug, Default)]
pub struct SseParser {
    /// Bytes received but not yet terminated by a line ending.
    pending: String,
    /// Accumulated `data:` fields for the event being built.
    data: Vec<String>,
    event_type: Option<String>,
    /// True when the last chunk ended exactly on a `\r`, so a `\n` opening the
    /// next chunk is that same line ending rather than a new blank line.
    pending_cr: bool,
}

impl SseParser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one chunk, returning every event it completed.
    pub fn push(&mut self, chunk: &str) -> Vec<SseEvent> {
        let mut out = Vec::new();
        let mut rest = chunk;

        // A `\n` at the very start of this chunk belongs to a `\r` that ended
        // the previous one; swallow it rather than reading an extra line.
        if self.pending_cr {
            self.pending_cr = false;
            if let Some(stripped) = rest.strip_prefix('\n') {
                rest = stripped;
            }
        }

        for ch in rest.chars() {
            match ch {
                '\n' => self.end_line(&mut out),
                '\r' => {
                    self.end_line(&mut out);
                    // If the chunk ends here, remember that a following `\n`
                    // is part of this same terminator.
                    self.pending_cr = true;
                }
                other => {
                    if self.pending_cr {
                        self.pending_cr = false;
                    }
                    self.pending.push(other);
                }
            }
        }
        out
    }

    /// Dispatch anything still buffered. Call when the connection closes: a
    /// well-behaved server ends with a blank line, but a truncated stream
    /// should still surrender whatever arrived.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.pending.is_empty() {
            self.end_line(&mut out);
        }
        if !self.data.is_empty() {
            self.dispatch(&mut out);
        }
        out
    }

    fn end_line(&mut self, out: &mut Vec<SseEvent>) {
        let line = std::mem::take(&mut self.pending);

        if line.is_empty() {
            // Blank line: dispatch, but only if something was accumulated.
            // Keep-alive comments produce blank lines with no data.
            if !self.data.is_empty() || self.event_type.is_some() {
                self.dispatch(out);
            }
            return;
        }
        if line.starts_with(':') {
            return; // comment / keep-alive
        }

        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            // A field with no colon is the field name with an empty value.
            None => (line.as_str(), ""),
        };

        match field {
            "event" => self.event_type = Some(value.to_owned()),
            "data" => self.data.push(value.to_owned()),
            // `id` and `retry` are not used here; ignore unknown fields.
            _ => {}
        }
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        let data = self.data.join("\n");
        let event = self.event_type.take();
        self.data.clear();
        out.push(SseEvent { event, data });
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn ev(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: Some(event.to_owned()),
            data: data.to_owned(),
        }
    }

    #[test]
    fn a_complete_event_parses() {
        let mut p = SseParser::new();
        let got = p.push("event: message_start\ndata: {\"a\":1}\n\n");
        assert_eq!(got, vec![ev("message_start", r#"{"a":1}"#)]);
    }

    #[test]
    fn an_event_split_across_chunks_is_reassembled() {
        let mut p = SseParser::new();
        // Boundaries chosen to fall mid-field, mid-value and mid-terminator.
        assert!(p.push("event: content_bl").is_empty());
        assert!(p.push("ock_delta\ndata: {\"te").is_empty());
        assert!(p.push("xt\":\"hi\"}").is_empty());
        let got = p.push("\n\n");
        assert_eq!(got, vec![ev("content_block_delta", r#"{"text":"hi"}"#)]);
    }

    #[test]
    fn a_chunk_split_between_cr_and_lf_does_not_dispatch_twice() {
        let mut p = SseParser::new();
        // The terminator itself is split: "...\r" | "\n\n".
        assert_eq!(p.push("data: one\r"), vec![]);
        let got = p.push("\n\n");
        assert_eq!(
            got,
            vec![SseEvent {
                event: None,
                data: "one".to_owned()
            }],
            "CRLF straddling a chunk boundary must count as one line ending"
        );
    }

    #[test]
    fn all_three_line_terminators_work() {
        for terminator in ["\n", "\r\n", "\r"] {
            let mut p = SseParser::new();
            let input = format!("data: x{terminator}{terminator}");
            let got = p.push(&input);
            assert_eq!(
                got,
                vec![SseEvent {
                    event: None,
                    data: "x".to_owned()
                }],
                "terminator {terminator:?} should work"
            );
        }
    }

    #[test]
    fn multiple_data_lines_join_with_newlines() {
        let mut p = SseParser::new();
        let got = p.push("data: line one\ndata: line two\n\n");
        assert_eq!(got[0].data, "line one\nline two");
    }

    #[test]
    fn comments_and_keepalives_are_ignored() {
        let mut p = SseParser::new();
        assert!(p.push(": keep-alive\n\n").is_empty());
        let got = p.push(": ping\ndata: real\n\n");
        assert_eq!(got[0].data, "real");
    }

    #[test]
    fn several_events_in_one_chunk_all_arrive_in_order() {
        let mut p = SseParser::new();
        let got = p.push("event: a\ndata: 1\n\nevent: b\ndata: 2\n\nevent: c\ndata: 3\n\n");
        assert_eq!(got, vec![ev("a", "1"), ev("b", "2"), ev("c", "3")]);
    }

    #[test]
    fn a_value_without_a_leading_space_keeps_its_characters() {
        let mut p = SseParser::new();
        let got = p.push("data:{\"tight\":true}\n\n");
        assert_eq!(got[0].data, r#"{"tight":true}"#);
    }

    #[test]
    fn only_the_first_space_after_the_colon_is_stripped() {
        let mut p = SseParser::new();
        let got = p.push("data:  two spaces\n\n");
        assert_eq!(got[0].data, " two spaces");
    }

    #[test]
    fn a_colon_inside_the_value_is_preserved() {
        let mut p = SseParser::new();
        let got = p.push("data: {\"url\":\"https://example.com\"}\n\n");
        assert_eq!(got[0].data, r#"{"url":"https://example.com"}"#);
    }

    #[test]
    fn a_truncated_stream_still_surrenders_what_arrived() {
        let mut p = SseParser::new();
        assert!(
            p.push("event: message_delta\ndata: {\"partial\":true}")
                .is_empty()
        );
        // Connection drops without the closing blank line.
        let got = p.finish();
        assert_eq!(got, vec![ev("message_delta", r#"{"partial":true}"#)]);
    }

    #[test]
    fn feeding_one_byte_at_a_time_gives_the_same_result_as_one_chunk() {
        let input = "event: content_block_delta\ndata: {\"text\":\"hello world\"}\n\nevent: message_stop\ndata: {}\n\n";

        let mut whole = SseParser::new();
        let expected = whole.push(input);

        let mut drip = SseParser::new();
        let mut got = Vec::new();
        // Split on char boundaries, one character per chunk.
        for ch in input.chars() {
            got.extend(drip.push(&ch.to_string()));
        }

        assert_eq!(got, expected, "chunking must not change the parse");
    }
}
