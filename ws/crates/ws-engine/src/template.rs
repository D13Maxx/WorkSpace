//! The GGUF-embedded chat template (master spec s8.8).
//!
//! A chat template is Jinja source that shipped inside a model file someone
//! downloaded, so it is untrusted input in the same sense the tensors are. It
//! is rendered in an environment with no filesystem loader, no network, a fuel
//! limit and a recursion limit, so a malformed or hostile template fails with
//! an error instead of hanging the engine.
//!
//! Getting this wrong is invisible rather than loud. A prompt assembled with
//! the wrong turn markers still produces fluent text; it is just text from a
//! model that no longer believes it is in a conversation. That is why M1
//! refuses to guess a template when the file has none.

use crate::error::{EngineError, Result};
use candle_core::quantized::gguf_file;
use minijinja::{Environment, Error as JinjaError, ErrorKind, Value, context};
use serde::{Deserialize, Serialize};

/// Instruction budget for one render. Chat templates are loops over a handful
/// of messages; anything approaching this is looping without terminating.
const RENDER_FUEL: u64 = 2_000_000;

/// Templates nest a few levels at most. The default (500) is far more than any
/// of them needs, and stack is shared with the model.
const RECURSION_LIMIT: usize = 32;

const TEMPLATE_NAME: &str = "chat";

/// One message as a chat template sees it.
///
/// Deliberately not [`crate::Part`]-shaped: templates index `message.content`
/// as a string, so multi-part content is flattened before it arrives here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateMessage {
    pub role: String,
    pub content: String,
}

impl TemplateMessage {
    #[must_use]
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }
}

#[derive(Debug)]
pub struct ChatTemplate {
    source: String,
    bos_token: String,
    eos_token: String,
}

impl ChatTemplate {
    /// Read the template out of GGUF metadata.
    ///
    /// # Errors
    /// Fails if the file embeds no `tokenizer.chat_template`. Master spec s8.8
    /// is explicit that M1 does not guess one.
    pub fn from_gguf(content: &gguf_file::Content, path: &str) -> Result<Self> {
        let source = content
            .metadata
            .get("tokenizer.chat_template")
            .and_then(|v| v.to_string().ok())
            .cloned()
            .ok_or_else(|| EngineError::NoChatTemplate {
                path: path.to_owned(),
            })?;

        Ok(Self {
            // Templates reference `bos_token` and `eos_token` directly. Left
            // undefined they render as empty, which silently drops the
            // begin-of-text marker rather than failing.
            bos_token: special_token_text(content, "tokenizer.ggml.bos_token_id"),
            eos_token: special_token_text(content, "tokenizer.ggml.eos_token_id"),
            source,
        })
    }

    /// Build from explicit source. For tests and for a user-supplied override.
    #[must_use]
    pub fn from_source(
        source: impl Into<String>,
        bos_token: impl Into<String>,
        eos_token: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            bos_token: bos_token.into(),
            eos_token: eos_token.into(),
        }
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Render a conversation into the exact text the model expects.
    ///
    /// `add_generation_prompt` writes the opening of the assistant's turn, so
    /// the model continues rather than starting a new user message.
    ///
    /// # Errors
    /// Fails if the template does not parse, calls `raise_exception`, or runs
    /// out of fuel.
    pub fn render(
        &self,
        messages: &[TemplateMessage],
        add_generation_prompt: bool,
    ) -> Result<String> {
        let mut env = sandboxed_environment();
        env.add_template(TEMPLATE_NAME, &self.source)
            .map_err(|e| EngineError::TemplateRender(describe(&e)))?;
        let tmpl = env
            .get_template(TEMPLATE_NAME)
            .map_err(|e| EngineError::TemplateRender(describe(&e)))?;

        tmpl.render(context! {
            messages => messages,
            add_generation_prompt => add_generation_prompt,
            bos_token => self.bos_token.as_str(),
            eos_token => self.eos_token.as_str(),
        })
        .map_err(|e| EngineError::TemplateRender(describe(&e)))
    }
}

/// An environment that can reach nothing outside itself.
///
/// No loader is registered (and the `loader` feature is not enabled), so
/// `include` and `extends` cannot read the filesystem. Fuel and recursion are
/// both bounded, so a runaway template returns an error rather than spinning.
fn sandboxed_environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_fuel(Some(RENDER_FUEL));
    env.set_recursion_limit(RECURSION_LIMIT);

    // Templates call this to reject inputs they cannot represent -- an
    // alternating-turns template given two user messages in a row, say. It has
    // to exist, or the template fails with "unknown function" and hides the
    // real complaint.
    env.add_function(
        "raise_exception",
        |message: String| -> std::result::Result<Value, JinjaError> {
            Err(JinjaError::new(ErrorKind::InvalidOperation, message))
        },
    );

    // Llama 3.x templates stamp a knowledge-cutoff line with this.
    env.add_function(
        "strftime_now",
        |format: String| -> std::result::Result<Value, JinjaError> {
            Ok(Value::from(strftime_now(&format)))
        },
    );

    env
}

/// The text of a special token, looked up through the vocabulary array.
fn special_token_text(content: &gguf_file::Content, id_key: &str) -> String {
    let Some(id) = content.metadata.get(id_key).and_then(|v| {
        v.to_u32()
            .ok()
            .or_else(|| v.to_u64().ok().and_then(|n| u32::try_from(n).ok()))
    }) else {
        return String::new();
    };
    content
        .metadata
        .get("tokenizer.ggml.tokens")
        .and_then(|v| v.to_vec().ok())
        .and_then(|tokens| tokens.get(id as usize).cloned())
        .and_then(|v| v.to_string().ok().cloned())
        .unwrap_or_default()
}

/// Minijinja reports the useful part of a failure in the source chain, not in
/// `Display`. A template that calls `raise_exception` otherwise surfaces as a
/// bare "invalid operation" with the model's own message discarded.
fn describe(err: &JinjaError) -> String {
    let mut out = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(e) = source {
        out.push_str(": ");
        out.push_str(&e.to_string());
        source = e.source();
    }
    out
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Today's date, formatted with the subset of strftime that chat templates use.
///
/// Hand-written rather than pulling in a date library for one call: templates
/// want a cutoff line like "26 July 2024", and the conversion from Unix days
/// to a civil date is a dozen lines of well-known integer arithmetic.
fn strftime_now(format: &str) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    let (year, month, day) = civil_from_days(days);
    let seconds_of_day = secs % 86_400;

    let mut out = String::with_capacity(format.len() + 8);
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('d') => out.push_str(&format!("{day:02}")),
            Some('e') => out.push_str(&format!("{day:2}")),
            Some('m') => out.push_str(&format!("{month:02}")),
            Some('Y') => out.push_str(&year.to_string()),
            Some('y') => out.push_str(&format!("{:02}", year % 100)),
            Some('B') => out.push_str(
                MONTHS
                    .get((month as usize).saturating_sub(1))
                    .unwrap_or(&""),
            ),
            Some('b') => out.push_str(
                MONTHS
                    .get((month as usize).saturating_sub(1))
                    .map_or("", |m| &m[..3]),
            ),
            Some('H') => out.push_str(&format!("{:02}", seconds_of_day / 3600)),
            Some('M') => out.push_str(&format!("{:02}", (seconds_of_day % 3600) / 60)),
            Some('S') => out.push_str(&format!("{:02}", seconds_of_day % 60)),
            Some('%') => out.push('%'),
            // Anything else is passed through rather than swallowed, so an
            // unhandled code is visible in the prompt instead of silent.
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Days since the Unix epoch to (year, month, day).
///
/// Hinnant's civil-from-days: shifts the era to start in March so the leap day
/// lands at the end of a year and needs no special case.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March = 0
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// The shape of the Llama 3 template, trimmed to what these tests exercise.
    const LLAMA3_ISH: &str = concat!(
        "{{- bos_token }}",
        "{%- for message in messages %}",
        "{{- '<|start_header_id|>' + message['role'] + '<|end_header_id|>\n\n' ",
        "+ message['content'] + '<|eot_id|>' }}",
        "{%- endfor %}",
        "{%- if add_generation_prompt %}",
        "{{- '<|start_header_id|>assistant<|end_header_id|>\n\n' }}",
        "{%- endif %}",
    );

    fn template() -> ChatTemplate {
        ChatTemplate::from_source(LLAMA3_ISH, "<|begin_of_text|>", "<|end_of_text|>")
    }

    fn conversation() -> Vec<TemplateMessage> {
        vec![
            TemplateMessage::new("system", "Be brief."),
            TemplateMessage::new("user", "Hello"),
        ]
    }

    #[test]
    fn a_conversation_renders_with_its_turn_markers() {
        let out = template().render(&conversation(), true).unwrap();
        assert!(out.starts_with("<|begin_of_text|>"), "got {out:?}");
        assert!(out.contains("<|start_header_id|>system<|end_header_id|>"));
        assert!(out.contains("Be brief."));
        assert!(out.contains("Hello"));
        assert_eq!(
            out.matches("<|eot_id|>").count(),
            2,
            "each message closes exactly once"
        );
    }

    #[test]
    fn the_generation_prompt_opens_the_assistant_turn() {
        let with = template().render(&conversation(), true).unwrap();
        let without = template().render(&conversation(), false).unwrap();

        assert!(with.ends_with("<|start_header_id|>assistant<|end_header_id|>\n\n"));
        assert!(!without.ends_with("<|start_header_id|>assistant<|end_header_id|>\n\n"));
        assert!(
            with.len() > without.len(),
            "the generation prompt only ever adds"
        );
    }

    #[test]
    fn the_bos_token_is_written_exactly_once() {
        // The template emits BOS itself. This is the half of the double-BOS
        // problem that lives here; the other half is in the tokenizer.
        let out = template().render(&conversation(), true).unwrap();
        assert_eq!(out.matches("<|begin_of_text|>").count(), 1);
    }

    #[test]
    fn raise_exception_surfaces_the_templates_own_message() {
        let t = ChatTemplate::from_source(
            "{{ raise_exception('Conversation roles must alternate') }}",
            "",
            "",
        );
        let err = t.render(&conversation(), true).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Conversation roles must alternate"),
            "the template's complaint must survive: {msg}"
        );
    }

    #[test]
    fn a_runaway_template_runs_out_of_fuel_rather_than_hanging() {
        // Without a fuel limit this never returns, and the engine thread is
        // gone for good (master spec s8.8).
        let t =
            ChatTemplate::from_source("{% for i in range(100000000) %}{{ i }}{% endfor %}", "", "");
        let err = t.render(&[], false).unwrap_err();
        assert!(matches!(err, EngineError::TemplateRender(_)), "got {err:?}");
    }

    #[test]
    fn a_template_cannot_read_the_filesystem() {
        // No loader is registered, so include has nothing to include from.
        for source in [
            "{% include 'C:/Windows/win.ini' %}",
            "{% extends '/etc/passwd' %}",
        ] {
            let t = ChatTemplate::from_source(source, "", "");
            assert!(
                t.render(&conversation(), false).is_err(),
                "{source} must not resolve"
            );
        }
    }

    #[test]
    fn a_template_that_does_not_parse_is_an_error_not_a_panic() {
        let t = ChatTemplate::from_source("{% for x in %}{{ x }}", "", "");
        assert!(t.render(&conversation(), true).is_err());
    }

    #[test]
    fn civil_dates_match_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1), "the epoch");
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29), "a leap day");
        assert_eq!(civil_from_days(20_454), (2026, 1, 1));
    }

    #[test]
    fn strftime_formats_the_codes_templates_use() {
        // Llama 3.2 asks for exactly this.
        let out = strftime_now("%d %B %Y");
        let parts: Vec<_> = out.split(' ').collect();
        assert_eq!(parts.len(), 3, "got {out:?}");
        assert_eq!(parts[0].len(), 2, "zero-padded day: {out:?}");
        assert!(MONTHS.contains(&parts[1]), "month name: {out:?}");
        assert!(
            parts[2]
                .parse::<i64>()
                .is_ok_and(|y| (2020..2100).contains(&y)),
            "plausible year: {out:?}"
        );
    }

    #[test]
    fn strftime_passes_through_codes_it_does_not_know() {
        assert_eq!(strftime_now("%Q"), "%Q", "unhandled codes stay visible");
        assert_eq!(strftime_now("100%%"), "100%");
    }
}
