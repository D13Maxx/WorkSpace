//! Retry rules shared by the API adapters (master spec s7.1).
//!
//! Retries happen only *before* the first streamed byte. That is not a policy
//! knob but a correctness rule: once a reply has begun arriving, re-sending
//! would duplicate the text the user already saw. The adapters enforce it
//! structurally by retrying around the initial POST only.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts, including the first. Spec s7.1 allows at most 3.
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(20),
        }
    }
}

impl RetryPolicy {
    /// Exponential backoff for `attempt` (1-based for the first retry).
    #[must_use]
    pub fn backoff(&self, attempt: u32) -> Duration {
        let factor = 1u32 << attempt.min(10).saturating_sub(1).min(10);
        let delay = self.base_delay.saturating_mul(factor);
        delay.min(self.max_delay)
    }

    /// What to wait before `attempt`, preferring the server's own instruction.
    ///
    /// A `retry-after` is capped at `max_delay`: a provider asking for several
    /// minutes should surface as an error the user can act on, not a hang.
    #[must_use]
    pub fn wait_for(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        match retry_after {
            Some(d) => d.min(self.max_delay),
            None => self.backoff(attempt),
        }
    }
}

/// Read `retry-after`, which may be either a count of seconds or an HTTP date.
#[must_use]
pub fn retry_after_from_headers(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    parse_retry_after(raw, chrono::Utc::now())
}

/// Split out from the header lookup so the date arm is testable with a fixed
/// clock instead of the wall clock.
#[must_use]
pub fn parse_retry_after(raw: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let when = chrono::DateTime::parse_from_rfc2822(raw).ok()?;
    let delta = when.with_timezone(&chrono::Utc) - now;
    delta.to_std().ok()
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn the_default_allows_at_most_three_attempts() {
        // Spec s7.1 sets this ceiling; a change here is a spec change.
        assert_eq!(RetryPolicy::default().max_attempts, 3);
    }

    #[test]
    fn backoff_grows_and_then_stops_growing() {
        let p = RetryPolicy::default();
        assert_eq!(p.backoff(1), Duration::from_millis(500));
        assert_eq!(p.backoff(2), Duration::from_secs(1));
        assert_eq!(p.backoff(3), Duration::from_secs(2));
        assert!(p.backoff(30) <= p.max_delay, "backoff must stay bounded");
    }

    #[test]
    fn a_server_instruction_wins_over_our_own_backoff() {
        let p = RetryPolicy::default();
        let wait = p.wait_for(1, Some(Duration::from_secs(7)));
        assert_eq!(wait, Duration::from_secs(7));
    }

    #[test]
    fn an_unreasonable_retry_after_is_capped_rather_than_obeyed() {
        let p = RetryPolicy::default();
        let wait = p.wait_for(1, Some(Duration::from_secs(3600)));
        assert_eq!(
            wait, p.max_delay,
            "a long retry-after should surface as an error, not a hang"
        );
    }

    #[test]
    fn retry_after_parses_a_count_of_seconds() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        assert_eq!(parse_retry_after("30", now), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after("  5 ", now), Some(Duration::from_secs(5)));
    }

    #[test]
    fn retry_after_parses_an_http_date() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        // now + 60s, expressed as an RFC 2822 date.
        let later = now + chrono::Duration::seconds(60);
        let raw = later.to_rfc2822();
        let got = parse_retry_after(&raw, now).unwrap();
        assert!(
            got.as_secs() >= 59 && got.as_secs() <= 60,
            "expected about 60s, got {got:?}"
        );
    }

    #[test]
    fn a_retry_after_date_in_the_past_yields_no_wait() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let earlier = (now - chrono::Duration::seconds(60)).to_rfc2822();
        assert_eq!(parse_retry_after(&earlier, now), None);
    }

    #[test]
    fn nonsense_is_ignored_rather_than_treated_as_zero() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        assert_eq!(parse_retry_after("soon", now), None);
    }
}
