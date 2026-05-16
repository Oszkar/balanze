//! Pure exponential-backoff policy + a generic async retry combinator.
//! No HTTP/provider types here — the caller supplies a `classify` closure
//! that inspects its own error. AGENTS.md §3.1 (start 30s, cap 10 min).
//! Single-user tool ⇒ no jitter (no thundering herd to spread).

use std::time::Duration;

/// Exponential-backoff schedule + retry budget.
#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    base: Duration,
    factor: u32,
    cap: Duration,
    max_retries: u32,
}

impl BackoffPolicy {
    /// §3.1 background-poller schedule: 30s, 60s, 120s, … capped at 10 min,
    /// 6 retries. For the future watcher's safety poll — NOT the one-shot CLI.
    pub fn standard() -> Self {
        Self {
            base: Duration::from_secs(30),
            factor: 2,
            cap: Duration::from_secs(600),
            max_retries: 6,
        }
    }

    /// One-shot CLI: never block a user-facing invocation on provider
    /// rate-limit backoff. Zero retries — surface the error immediately.
    pub fn fail_fast() -> Self {
        Self {
            base: Duration::from_secs(0),
            factor: 2,
            cap: Duration::from_secs(0),
            max_retries: 0,
        }
    }

    /// Escape hatch / test affordance. Note: `base = 0` (or `cap = 0`) with
    /// `RetryDecision::RetryAfter(None)` yields immediate (zero-delay) retries
    /// bounded by `max_retries` — intended for tests, not production polling.
    pub fn custom(base: Duration, factor: u32, cap: Duration, max_retries: u32) -> Self {
        Self {
            base,
            factor,
            cap,
            max_retries,
        }
    }

    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }

    pub fn cap(&self) -> Duration {
        self.cap
    }

    /// Backoff delay before retry `attempt` (0-indexed): `base * factor^attempt`,
    /// saturating to `cap`. Sub-second `base` is preserved (Duration math, not
    /// whole-second truncation). Overflow (huge `attempt`) saturates to `cap`,
    /// never panics.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        match self.factor.checked_pow(attempt) {
            None => self.cap, // factor^attempt overflowed u32 ⇒ astronomically large ⇒ cap
            Some(mult) => match self.base.checked_mul(mult) {
                None => self.cap, // base*mult overflowed Duration ⇒ cap
                Some(d) => d.min(self.cap),
            },
        }
    }
}

/// What `retry` should do with a given error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Permanent failure — return it immediately.
    DoNotRetry,
    /// Transient — retry. `Some(d)` = server-suggested delay (e.g. parsed
    /// `Retry-After`), used instead of the schedule (clamped to the cap).
    /// `None` = use the policy schedule.
    RetryAfter(Option<Duration>),
}

/// Run `op`, retrying transient failures per `policy`. Pure scheduling +
/// `tokio::time::sleep`; deterministic under `tokio::time::pause()`.
pub async fn retry<T, E, Op, Fut>(
    policy: &BackoffPolicy,
    classify: impl Fn(&E) -> RetryDecision,
    mut op: Op,
) -> Result<T, E>
where
    Op: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut attempt: u32 = 0;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => match classify(&e) {
                RetryDecision::DoNotRetry => return Err(e),
                RetryDecision::RetryAfter(_) if attempt >= policy.max_retries => return Err(e),
                RetryDecision::RetryAfter(server) => {
                    let delay = match server {
                        Some(d) => d.min(policy.cap()),
                        None => policy.delay_for_attempt(attempt),
                    };
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    #[test]
    fn delay_schedule_and_saturation() {
        let p = BackoffPolicy::standard();
        assert_eq!(p.delay_for_attempt(0), Duration::from_secs(30));
        assert_eq!(p.delay_for_attempt(1), Duration::from_secs(60));
        assert_eq!(p.delay_for_attempt(2), Duration::from_secs(120));
        assert_eq!(p.delay_for_attempt(4), Duration::from_secs(480));
        assert_eq!(p.delay_for_attempt(5), Duration::from_secs(600)); // capped (30*32=960>600)
        assert_eq!(p.delay_for_attempt(99), Duration::from_secs(600)); // saturates, no panic
    }

    #[tokio::test(start_paused = true)]
    async fn fail_fast_calls_op_once_and_returns_err() {
        let calls = AtomicU32::new(0);
        let r: Result<(), &str> = retry(
            &BackoffPolicy::fail_fast(),
            |_| RetryDecision::RetryAfter(None),
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err("boom") }
            },
        )
        .await;
        assert_eq!(r, Err("boom"));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "fail_fast ⇒ no retries");
    }

    #[tokio::test(start_paused = true)]
    async fn retries_then_succeeds_under_standard() {
        let calls = AtomicU32::new(0);
        let r: Result<u32, &str> = retry(
            &BackoffPolicy::standard(),
            |_| RetryDecision::RetryAfter(None),
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 3 {
                        Err("transient")
                    } else {
                        Ok(n)
                    }
                }
            },
        )
        .await;
        assert_eq!(r, Ok(3));
        assert_eq!(calls.load(Ordering::SeqCst), 4, "3 retries then success");
    }

    #[tokio::test(start_paused = true)]
    async fn do_not_retry_returns_immediately() {
        let calls = AtomicU32::new(0);
        let r: Result<(), &str> = retry(
            &BackoffPolicy::standard(),
            |_| RetryDecision::DoNotRetry,
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err("fatal") }
            },
        )
        .await;
        assert_eq!(r, Err("fatal"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn server_retry_after_is_honored_and_capped() {
        let start = tokio::time::Instant::now();
        let calls = AtomicU32::new(0);
        let _ = retry::<(), &str, _, _>(
            &BackoffPolicy::standard(),
            |_| RetryDecision::RetryAfter(Some(Duration::from_secs(5))),
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n == 0 {
                        Err("rate limited")
                    } else {
                        Ok(())
                    }
                }
            },
        )
        .await;
        // One retry, slept the server-suggested 5s (not the 30s schedule).
        assert_eq!(start.elapsed(), Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn exhaustion_returns_the_real_last_error_not_a_wrapper() {
        // Track A's 401→refresh and the CLI error surface depend on `retry`
        // surfacing the genuine final error (e.g. AuthExpired), never a
        // synthetic "retries exhausted". Pin it with a sentinel value.
        let policy = BackoffPolicy::custom(Duration::ZERO, 2, Duration::ZERO, 2);
        let r: Result<(), u32> = retry(
            &policy,
            |_| RetryDecision::RetryAfter(None),
            || async { Err(424242_u32) },
        )
        .await;
        assert_eq!(r, Err(424242_u32), "must return the original error value");
    }

    #[tokio::test(start_paused = true)]
    async fn server_retry_after_exceeding_cap_is_clamped_to_cap() {
        // A hostile/large `Retry-After: 86400` must not park the watcher for
        // a day — the server delay is clamped to the policy cap.
        let start = tokio::time::Instant::now();
        let calls = std::sync::atomic::AtomicU32::new(0);
        let policy = BackoffPolicy::standard(); // cap = 600s
        let _ = retry::<(), &str, _, _>(
            &policy,
            |_| RetryDecision::RetryAfter(Some(Duration::from_secs(86_400))),
            || {
                let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    if n == 0 {
                        Err("rate limited")
                    } else {
                        Ok(())
                    }
                }
            },
        )
        .await;
        assert_eq!(
            start.elapsed(),
            Duration::from_secs(600),
            "server Retry-After above cap must clamp to the 600s cap"
        );
    }

    #[test]
    fn custom_sub_second_base_is_preserved_not_truncated() {
        // Regression: `base.as_secs()` silently zeroed sub-second policies.
        let p = BackoffPolicy::custom(Duration::from_millis(500), 2, Duration::from_secs(10), 5);
        assert_eq!(p.delay_for_attempt(0), Duration::from_millis(500));
        assert_eq!(p.delay_for_attempt(1), Duration::from_secs(1));
        assert_eq!(p.delay_for_attempt(2), Duration::from_secs(2));
        assert_eq!(p.delay_for_attempt(3), Duration::from_secs(4));
        // 500ms * 2^5 = 16s, capped at 10s:
        assert_eq!(p.delay_for_attempt(5), Duration::from_secs(10));
        // Huge attempt saturates to cap, no panic:
        assert_eq!(p.delay_for_attempt(99), Duration::from_secs(10));
    }
}
