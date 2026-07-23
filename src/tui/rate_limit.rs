//! Rate-limit (HTTP 429) handling for the Spotify Web API.
//!
//! Spotify aggregates requests in a rolling ~30-second window and answers `429 Too Many Requests`
//! when a client bursts past its budget, usually with a `Retry-After` header (integer seconds).
//! rspotify 0.16.1 surfaces this as `ClientError::Http(HttpError::StatusCode(resp))`, carrying the
//! un-consumed `reqwest::Response` (status + headers, body unread). This module turns such an error
//! into a typed [`RateLimitHit`], computes how long to wait, and is the single source of the 429
//! status-line wording.
//!
//! All the decision logic here is pure (seconds in, seconds/`String` out) so it is unit-tested
//! without constructing rspotify models — which are impractical to build in tests. The stateful
//! cooldown (`rate_limited_until` / `rate_limit_hits`) lives on `super::App`; this module only
//! detects hits and does the arithmetic.

use std::time::{Duration, Instant};

use rspotify::ClientError;
use rspotify::http::HttpError;

use crate::theme;

/// HTTP status Spotify returns when the rolling request budget is exceeded.
const TOO_MANY_REQUESTS: u16 = 429;
/// Upper bound applied to a server `Retry-After`. Spotify's values are small; this guards against a
/// bogus/huge value freezing the client for hours. A server value is honored up to this cap (it is
/// *not* shortened to the local backoff cap — the server knows best when it is safe to retry).
const RETRY_AFTER_MAX_SECS: u64 = 300;
/// Base of the local exponential backoff used when the server sends no usable `Retry-After`.
const BACKOFF_BASE_SECS: u64 = 2;
/// Cap of the *local* exponential backoff (does not apply to a server `Retry-After`).
const BACKOFF_CAP_SECS: u64 = 60;

/// A detected 429. `retry_after` is the server-provided wait (seconds, already sanity-capped) when
/// the header is present and parseable as an integer; `None` when it is absent or an HTTP-date, in
/// which case the caller falls back to the local exponential backoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RateLimitHit {
    pub(super) retry_after: Option<u64>,
}

/// The un-consumed HTTP response behind a client error, when the failure was an HTTP status (rather
/// than a transport or parse error). The single place that pattern-matches rspotify's error shape,
/// shared by [`http_status`] and [`detect_client_error`].
fn http_response(err: &ClientError) -> Option<&reqwest::Response> {
    match err {
        ClientError::Http(http) => match http.as_ref() {
            HttpError::StatusCode(resp) => Some(resp),
            _ => None,
        },
        _ => None,
    }
}

/// The HTTP status of a client error, when the failure was an HTTP response (rather than a transport
/// or parse error). Shared with the detail pane's `403` content-restriction handling.
pub(super) fn http_status(err: &ClientError) -> Option<u16> {
    http_response(err).map(|resp| resp.status().as_u16())
}

/// Parse a `Retry-After` header value. Only integer seconds are honored; the HTTP-date form and any
/// non-numeric value yield `None` (the caller then uses the local backoff). The result is clamped to
/// [`RETRY_AFTER_MAX_SECS`].
pub(super) fn parse_retry_after(value: &str) -> Option<u64> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|s| s.min(RETRY_AFTER_MAX_SECS))
}

/// The local exponential backoff for the `hits`-th consecutive 429 (1-based): `base * 2^(hits-1)`,
/// clamped to [`BACKOFF_CAP_SECS`]. `hits == 0` is treated as the first hit. The exponent is bounded
/// before shifting so it can never overflow regardless of how long a rate limit persists.
pub(super) fn backoff_secs(hits: u32) -> u64 {
    // A cap of 60s is reached by `2 * 2^5`, so clamping the exponent at 6 is more than enough and
    // keeps the shift trivially in range.
    let exp = hits.saturating_sub(1).min(6);
    (BACKOFF_BASE_SECS << exp).min(BACKOFF_CAP_SECS)
}

/// How long to wait after a 429: the server `Retry-After` when present (already capped), otherwise
/// the local exponential backoff for the current consecutive-hit count.
pub(super) fn wait_secs(retry_after: Option<u64>, hits: u32) -> u64 {
    retry_after.unwrap_or_else(|| backoff_secs(hits))
}

/// Classify a `ClientError` as a 429, extracting the server `Retry-After` if any. `None` when the
/// error is not an HTTP 429 (a different status, or a transport/parse error).
pub(super) fn detect_client_error(err: &ClientError) -> Option<RateLimitHit> {
    let resp = http_response(err)?;
    if resp.status().as_u16() != TOO_MANY_REQUESTS {
        return None;
    }
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after);
    Some(RateLimitHit { retry_after })
}

/// Find a 429 anywhere in an `anyhow` error chain. The playback/fetch paths wrap the `ClientError`
/// with `.context(...)`, so the typed error is a *source*, not the head; `chain()` walks the whole
/// cause chain and `downcast_ref` recovers the concrete type. The relied-on invariant (that a typed
/// error survives `.context()` and is reachable this way) is pinned by a unit test below.
pub(super) fn detect(err: &anyhow::Error) -> Option<RateLimitHit> {
    err.chain()
        .find_map(|e| e.downcast_ref::<ClientError>())
        .and_then(detect_client_error)
}

/// Remaining cooldown at `now`, or `None` when not (or no longer) blocked. Taking `now` as a
/// parameter keeps this testable without sleeping.
pub(super) fn remaining(until: Option<Instant>, now: Instant) -> Option<Duration> {
    until
        .and_then(|t| t.checked_duration_since(now))
        .filter(|d| !d.is_zero())
}

/// The rate-limit countdown shown on the status line. Starts with [`theme::WARN`] so
/// `view::status_kind` classifies it as a warning (red). The wording deliberately avoids the
/// `OK_PREFIXES` glyphs so the status stays red for the whole countdown.
pub(super) fn rate_limit_status(remaining_secs: u64) -> String {
    format!(
        "{} Rate limited by Spotify — retrying in {remaining_secs}s",
        theme::WARN
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::view::{self, StatusKind};

    #[test]
    fn parse_retry_after_reads_integer_seconds() {
        assert_eq!(parse_retry_after("5"), Some(5));
        assert_eq!(parse_retry_after("  12 "), Some(12));
        assert_eq!(parse_retry_after("0"), Some(0));
    }

    #[test]
    fn parse_retry_after_rejects_non_integers_and_http_dates() {
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("1.5"), None);
        assert_eq!(parse_retry_after(""), None);
        assert_eq!(parse_retry_after("-3"), None);
    }

    #[test]
    fn parse_retry_after_caps_absurd_values() {
        assert_eq!(parse_retry_after("100000"), Some(RETRY_AFTER_MAX_SECS));
    }

    #[test]
    fn backoff_grows_exponentially_then_caps() {
        assert_eq!(backoff_secs(0), 2); // treated as first hit
        assert_eq!(backoff_secs(1), 2);
        assert_eq!(backoff_secs(2), 4);
        assert_eq!(backoff_secs(3), 8);
        assert_eq!(backoff_secs(4), 16);
        assert_eq!(backoff_secs(5), 32);
        assert_eq!(backoff_secs(6), 60); // 64 clamped to the 60s cap
        assert_eq!(backoff_secs(100), 60); // stays capped, never overflows
    }

    #[test]
    fn wait_prefers_server_retry_after_over_local_backoff() {
        assert_eq!(wait_secs(Some(7), 5), 7);
        assert_eq!(wait_secs(None, 3), backoff_secs(3));
    }

    #[test]
    fn remaining_is_none_once_the_deadline_passes() {
        let now = Instant::now();
        assert_eq!(remaining(None, now), None);
        assert!(remaining(Some(now + Duration::from_secs(5)), now).is_some());
        // A deadline at or before `now` is no longer blocking.
        assert_eq!(remaining(Some(now), now), None);
    }

    #[test]
    fn rate_limit_status_is_classified_as_a_warning() {
        // Contract with the status-line color coding: the countdown must read as a warning (red),
        // never as a success message.
        assert_eq!(view::status_kind(&rate_limit_status(30)), StatusKind::Warn);
    }

    // A stand-in error standing for `rspotify::ClientError`: `detect` cannot be exercised with a real
    // `ClientError` (its 429 variant wraps a live `reqwest::Response` that is impractical to build in
    // a unit test), so this pins the *mechanism* `detect` depends on — that a typed error wrapped in
    // `anyhow`'s `.context(...)` is still recoverable via `chain().find_map(downcast_ref)`.
    #[derive(Debug)]
    struct StandInError;
    impl std::fmt::Display for StandInError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "stand-in error")
        }
    }
    impl std::error::Error for StandInError {}

    #[test]
    fn anyhow_context_keeps_the_typed_source_downcastable() {
        let err = anyhow::Error::new(StandInError).context("failed to reach Spotify");
        let found = err.chain().find_map(|e| e.downcast_ref::<StandInError>());
        assert!(
            found.is_some(),
            "a typed error must remain reachable through .context() for `detect` to work"
        );
    }
}
