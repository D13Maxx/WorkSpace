//! Shared HTTP plumbing for the API adapters (master spec s7.1).

use std::time::Duration;
use ws_context::{BackendError, BackendId};

use crate::retry::{RetryPolicy, retry_after_from_headers};

/// One header to send, as a borrowed pair.
pub type Header<'a> = (&'a str, String);

/// POST `body` to `url`, retrying only while nothing has been streamed.
///
/// Master spec s7.1 allows retries on 429, 5xx and connect errors, at most
/// `policy.max_attempts` times, honouring `retry-after`. Because this returns
/// before the response body is read, no partial reply can be duplicated: the
/// retry window structurally cannot overlap the stream.
///
/// # Errors
/// Returns the last failure once attempts are exhausted, or immediately for a
/// failure that retrying cannot fix.
pub async fn post_with_retry(
    http: &reqwest::Client,
    backend: &BackendId,
    url: &str,
    headers: Vec<Header<'_>>,
    body: &serde_json::Value,
    policy: RetryPolicy,
    summarise: fn(&str) -> String,
) -> Result<reqwest::Response, BackendError> {
    let mut last: Option<BackendError> = None;
    let mut retry_after: Option<Duration> = None;

    for attempt in 0..policy.max_attempts {
        if attempt > 0 {
            let wait = policy.wait_for(attempt, retry_after);
            tracing::debug!(%backend, attempt, ?wait, "retrying request");
            tokio::time::sleep(wait).await;
        }
        // Only the shape of the request is logged, never its body or headers:
        // both carry user content and the API key (s7.1).
        tracing::debug!(%backend, attempt, "posting");

        let mut builder = http.post(url).header("content-type", "application/json");
        for (name, value) in &headers {
            builder = builder.header(*name, value);
        }

        match builder.json(body).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(resp),
            Ok(resp) => {
                let status = resp.status().as_u16();
                let suggested = retry_after_from_headers(resp.headers());
                let text = resp.text().await.unwrap_or_default();
                let err = BackendError::Http {
                    backend: backend.clone(),
                    status,
                    message: summarise(&text),
                };
                if !err.is_retryable() {
                    return Err(err);
                }
                last = Some(err);
                retry_after = suggested;
            }
            Err(e) => {
                let err = BackendError::Transport {
                    backend: backend.clone(),
                    message: e.to_string(),
                };
                if !err.is_retryable() {
                    return Err(err);
                }
                last = Some(err);
                retry_after = None;
            }
        }
    }

    Err(last.unwrap_or_else(|| BackendError::Transport {
        backend: backend.clone(),
        message: "request failed after retries".to_owned(),
    }))
}

/// Pull a readable message out of an error body, falling back to the raw text.
///
/// Tries the shapes the supported providers actually use. Never includes
/// headers, which carry the API key.
#[must_use]
pub fn summarise_error(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            for pointer in ["/error/message", "/error/0/message", "/message", "/detail"] {
                if let Some(m) = v.pointer(pointer).and_then(|m| m.as_str()) {
                    return Some(m.to_owned());
                }
            }
            None
        })
        .unwrap_or_else(|| body.chars().take(500).collect())
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_anthropic_error_shape_is_understood() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens is too large"}}"#;
        assert_eq!(summarise_error(body), "max_tokens is too large");
    }

    #[test]
    fn the_openai_error_shape_is_understood() {
        let body = r#"{"error":{"message":"Unknown parameter: 'foo'.","type":"invalid_request_error","code":null}}"#;
        assert_eq!(summarise_error(body), "Unknown parameter: 'foo'.");
    }

    #[test]
    fn the_gemini_error_shape_is_understood() {
        let body =
            r#"{"error":{"code":400,"message":"API key not valid","status":"INVALID_ARGUMENT"}}"#;
        assert_eq!(summarise_error(body), "API key not valid");
    }

    #[test]
    fn an_unparseable_body_is_truncated_rather_than_dropped() {
        let body = "x".repeat(5000);
        assert_eq!(summarise_error(&body).len(), 500);
    }

    #[test]
    fn an_html_error_page_still_yields_something_readable() {
        // A proxy or gateway in front of the API can return HTML.
        let body = "<html><body><h1>502 Bad Gateway</h1></body></html>";
        assert!(summarise_error(body).contains("502"));
    }
}
