//! Generic HTTP retry infrastructure shared by every reqwest-based adapter.
//!
//! Both the AI providers (`ai_providers/`) and the Turbopuffer store
//! (`store/turbopuffer.rs`) talk to remote HTTP APIs with the same shape of
//! resilience: a bounded number of retries with exponential backoff on
//! transient send failures and retryable status codes. This module owns that
//! loop so the adapters don't each reimplement it.
//!
//! [`send_with_retry`] returns the final [`reqwest::Response`] — it does NOT
//! interpret the body. Callers inspect the status and parse (or bail) as they
//! see fit; only transport errors that persist past the retry budget surface
//! as an `Err`.

use std::time::Duration;

use anyhow::{Context, Result};

/// Retry classification + timing for a family of requests.
///
/// `retryable` is a status-code predicate: when it returns `true` for a
/// response's status and the retry budget isn't exhausted, the request is
/// retried after a backoff. Construct via [`RetryPolicy::standard`] or
/// [`RetryPolicy::server_errors`].
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_retries: usize,
    pub base_delay_ms: u64,
    pub retryable: fn(u16) -> bool,
}

impl RetryPolicy {
    /// Retry on rate limits and the common transient server statuses
    /// (429, 500, 502, 503, 529). Used by the hosted AI APIs.
    pub fn standard(max_retries: usize, base_delay_ms: u64) -> Self {
        Self {
            max_retries,
            base_delay_ms,
            retryable: is_retryable_standard,
        }
    }

    /// Retry on any 5xx server error. Used by Ollama (local) and the
    /// Turbopuffer store, which treat client errors as terminal.
    pub fn server_errors(max_retries: usize, base_delay_ms: u64) -> Self {
        Self {
            max_retries,
            base_delay_ms,
            retryable: is_server_error,
        }
    }
}

/// The standard retryable set for hosted AI APIs: rate limiting plus the
/// transient server statuses (including Anthropic's 529 "overloaded").
pub fn is_retryable_standard(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 529)
}

/// Any 5xx status.
pub fn is_server_error(status: u16) -> bool {
    status >= 500
}

/// Exponential backoff: `base_delay_ms * 2^attempt`.
pub fn backoff(policy: &RetryPolicy, attempt: usize) -> Duration {
    Duration::from_millis(policy.base_delay_ms * 2u64.pow(attempt as u32))
}

/// Send a request with bounded exponential-backoff retry.
///
/// `build` is called once per attempt to construct a fresh
/// [`reqwest::RequestBuilder`] (a builder is consumed by `send`, so it can't
/// be reused across attempts). `label` is used only in the error context and
/// retry warnings.
///
/// Returns the response as soon as a non-retryable status is seen or the
/// retry budget is exhausted — the caller is responsible for inspecting the
/// status and reading/parsing the body. Only a transport error that survives
/// every retry is returned as `Err`.
pub async fn send_with_retry<F>(
    policy: &RetryPolicy,
    label: &str,
    build: F,
) -> Result<reqwest::Response>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    let mut attempt = 0;
    loop {
        match build().send().await {
            Ok(resp) => {
                if (policy.retryable)(resp.status().as_u16()) && attempt < policy.max_retries {
                    let delay = backoff(policy, attempt);
                    eprintln!(
                        "warning: {label} returned {} (attempt {}), retrying in {delay:?}",
                        resp.status(),
                        attempt + 1
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                    continue;
                }
                return Ok(resp);
            }
            Err(e) => {
                if attempt < policy.max_retries {
                    let delay = backoff(policy, attempt);
                    eprintln!(
                        "warning: {label} request failed (attempt {}): {e}, retrying in {delay:?}",
                        attempt + 1
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                    continue;
                }
                return Err(e).with_context(|| format!("{label} request failed"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_retryable_set() {
        assert!(is_retryable_standard(429));
        assert!(is_retryable_standard(500));
        assert!(is_retryable_standard(502));
        assert!(is_retryable_standard(503));
        assert!(is_retryable_standard(529));
        assert!(!is_retryable_standard(200));
        assert!(!is_retryable_standard(400));
        assert!(!is_retryable_standard(401));
        assert!(!is_retryable_standard(404));
    }

    #[test]
    fn server_error_set() {
        assert!(is_server_error(500));
        assert!(is_server_error(502));
        assert!(is_server_error(599));
        assert!(!is_server_error(200));
        assert!(!is_server_error(429));
        assert!(!is_server_error(404));
    }

    #[test]
    fn backoff_is_exponential() {
        let p = RetryPolicy::standard(3, 1000);
        assert_eq!(backoff(&p, 0), Duration::from_secs(1));
        assert_eq!(backoff(&p, 1), Duration::from_secs(2));
        assert_eq!(backoff(&p, 2), Duration::from_secs(4));
        assert_eq!(backoff(&p, 3), Duration::from_secs(8));
    }

    #[test]
    fn backoff_respects_base_delay() {
        let p = RetryPolicy::server_errors(3, 500);
        assert_eq!(backoff(&p, 0), Duration::from_millis(500));
        assert_eq!(backoff(&p, 1), Duration::from_secs(1));
        assert_eq!(backoff(&p, 2), Duration::from_secs(2));
    }

    #[test]
    fn constructors_wire_the_right_predicate() {
        let std = RetryPolicy::standard(3, 1000);
        assert!((std.retryable)(429));
        let srv = RetryPolicy::server_errors(3, 1000);
        assert!(!(srv.retryable)(429));
        assert!((srv.retryable)(500));
    }
}
