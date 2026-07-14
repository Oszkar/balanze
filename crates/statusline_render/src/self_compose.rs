//! Self-compose path: build cross-provider segments without the watcher.
//!
//! The statusline calls this only when there is no fresh `snapshot.json`. It
//! reads Codex locally (cheap, every turn) and serves the OpenAI cost figure
//! through the cache (cache.rs) so the billing API is hit at most once per 300s
//! (AGENTS.md 3.1). It calls ONLY the two sources behind `CrossSources` - never
//! the Anthropic OAuth path (AGENTS.md §3.1 politeness invariant); that is why this crate
//! has no `anthropic_oauth` dependency.

use std::path::Path;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};

use crate::cache;
use crate::render::CrossProvider;

/// A prompt with no cached value waits briefly for the process currently
/// refreshing it. A prompt with stale data never waits: it returns stale data
/// immediately. The HTTP owner has its own 3-second timeout.
const REFRESH_WAIT_TIMEOUT: StdDuration = StdDuration::from_millis(250);
const REFRESH_WAIT_POLL: StdDuration = StdDuration::from_millis(20);

/// The two cross-provider sources, abstracted so the orchestrator (and its
/// once-per-300s gate) is testable without network. The real implementation
/// lives in `balanze_cli` (`LiveCrossSources`).
// Static dispatch only; async-fn-in-trait is stable and safe here.
// See snapshot_composer::SnapshotSources for the full rationale.
#[allow(async_fn_in_trait)]
pub trait CrossSources {
    /// `Ok(Some(v))` = fetched v (micro-USD); `Ok(None)` = no OpenAI key
    /// configured (no cell, no cooldown); `Err(msg)` = the fetch attempt failed
    /// (triggers the negative cooldown and keeps any prior value).
    async fn fetch_openai_total_micro_usd(&self) -> Result<Option<i64>, String>;
    /// Local Codex window utilizations (0..100): `(five_hour, weekly)`. Each
    /// `None` if that window is absent/unparsed or not present on the plan.
    fn codex_windows(&self) -> (Option<f32>, Option<f32>);
}

/// Compose the cross-provider cells without the watcher.
///
/// `want_openai` is false when no configured statusline line contains the
/// `{openai_cost}` placeholder. In that case the OpenAI cost is not fetched at
/// all - not from the cache, not from the network. The politest call to a
/// provider is the one you do not make (AGENTS.md §3.1).
pub async fn self_compose<S: CrossSources>(
    sources: &S,
    cache_dir: &Path,
    fingerprint: &str,
    now: DateTime<Utc>,
    want_openai: bool,
) -> CrossProvider {
    // Codex: local, cheap, never cached -> current whenever present.
    let (codex_five_hour, codex_weekly) = sources.codex_windows();

    let (openai_cost_micro_usd, openai_stale) = if want_openai {
        openai_value(sources, cache_dir, fingerprint, now).await
    } else {
        (None, false)
    };

    CrossProvider {
        codex_five_hour,
        codex_weekly,
        openai_cost_micro_usd,
        codex_stale: false,
        openai_stale,
    }
}

async fn openai_value<S: CrossSources>(
    sources: &S,
    cache_dir: &Path,
    fingerprint: &str,
    now: DateTime<Utc>,
) -> (Option<i64>, bool) {
    let initial = cache::read(cache_dir, fingerprint);
    if let Some(value) = usable_cached_value(initial.as_ref(), now) {
        return value;
    }
    let stale = initial.as_ref().and_then(|entry| entry.total_micro_usd);

    match cache::try_acquire_refresh_lease(cache_dir) {
        Ok(cache::LeaseAttempt::Acquired(lease)) => {
            refresh_under_lease(sources, cache_dir, fingerprint, now, lease).await
        }
        Ok(cache::LeaseAttempt::Busy) if stale.is_some() => (stale, true),
        Ok(cache::LeaseAttempt::Busy) => {
            wait_for_refresh_or_lease(sources, cache_dir, fingerprint, now).await
        }
        Err(error) => {
            tracing::debug!("statusline cache lease acquisition failed: {error}");
            (stale, stale.is_some())
        }
    }
}

/// Recheck the cache after acquisition. Another process may have published
/// between our optimistic read and successful `create_new`, so this check is
/// what turns the lease into an at-most-one-refresh gate rather than merely an
/// at-most-one-concurrent-request gate.
async fn refresh_under_lease<S: CrossSources>(
    sources: &S,
    cache_dir: &Path,
    fingerprint: &str,
    now: DateTime<Utc>,
    _lease: cache::RefreshLease,
) -> (Option<i64>, bool) {
    let current = cache::read(cache_dir, fingerprint);
    if let Some(value) = usable_cached_value(current.as_ref(), now) {
        return value;
    }
    let stale = current.as_ref().and_then(|entry| entry.total_micro_usd);

    match sources.fetch_openai_total_micro_usd().await {
        Ok(Some(value)) => {
            cache::write_success(cache_dir, fingerprint, value, now);
            (Some(value), false)
        }
        Ok(None) => (None, false),
        Err(error) => {
            tracing::debug!("statusline: OpenAI self-compose fetch failed: {error}");
            cache::write_failure(cache_dir, fingerprint, now);
            (stale, stale.is_some())
        }
    }
}

async fn wait_for_refresh_or_lease<S: CrossSources>(
    sources: &S,
    cache_dir: &Path,
    fingerprint: &str,
    now: DateTime<Utc>,
) -> (Option<i64>, bool) {
    let deadline = tokio::time::Instant::now() + REFRESH_WAIT_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return (None, false);
        }
        tokio::time::sleep(remaining.min(REFRESH_WAIT_POLL)).await;

        let current = cache::read(cache_dir, fingerprint);
        if let Some(value) = usable_cached_value(current.as_ref(), now) {
            return value;
        }
        let stale = current.as_ref().and_then(|entry| entry.total_micro_usd);
        if stale.is_some() {
            return (stale, true);
        }

        match cache::try_acquire_refresh_lease(cache_dir) {
            Ok(cache::LeaseAttempt::Acquired(lease)) => {
                return refresh_under_lease(sources, cache_dir, fingerprint, now, lease).await;
            }
            Ok(cache::LeaseAttempt::Busy) => {}
            Err(error) => {
                tracing::debug!("statusline cache lease acquisition failed: {error}");
                return (None, false);
            }
        }
    }
}

fn usable_cached_value(
    entry: Option<&cache::OpenAiCostEntry>,
    now: DateTime<Utc>,
) -> Option<(Option<i64>, bool)> {
    let entry = entry?;
    if cache::is_fresh(entry, now) {
        Some((entry.total_micro_usd, false))
    } else if cache::in_cooldown(entry, now) {
        Some((entry.total_micro_usd, entry.total_micro_usd.is_some()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache;
    use chrono::{Duration, TimeZone as _, Utc};
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    fn t0() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).unwrap()
    }

    struct Fake {
        openai: Result<Option<i64>, String>,
        codex: Option<f32>,
        calls: Cell<u32>,
    }
    impl CrossSources for Fake {
        async fn fetch_openai_total_micro_usd(&self) -> Result<Option<i64>, String> {
            self.calls.set(self.calls.get() + 1);
            self.openai.clone()
        }
        fn codex_windows(&self) -> (Option<f32>, Option<f32>) {
            (self.codex, None)
        }
    }

    #[tokio::test]
    async fn empty_cache_fetches_once_and_caches() {
        let dir = tempdir().unwrap();
        let f = Fake {
            openai: Ok(Some(4_200_000)),
            codex: Some(6.0),
            calls: Cell::new(0),
        };
        let cp = self_compose(&f, dir.path(), "fp", t0(), true).await;
        assert_eq!(cp.openai_cost_micro_usd, Some(4_200_000));
        assert_eq!(cp.codex_five_hour, Some(6.0));
        assert!(!cp.openai_stale && !cp.codex_stale);
        assert_eq!(f.calls.get(), 1);
        assert!(cache::read(dir.path(), "fp").is_some(), "value cached");
    }

    #[tokio::test]
    async fn second_call_within_ttl_does_not_refetch() {
        let dir = tempdir().unwrap();
        let f = Fake {
            openai: Ok(Some(10)),
            codex: None,
            calls: Cell::new(0),
        };
        let _ = self_compose(&f, dir.path(), "fp", t0(), true).await;
        let cp = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(120), true).await;
        assert_eq!(cp.openai_cost_micro_usd, Some(10));
        assert!(!cp.openai_stale);
        assert_eq!(f.calls.get(), 1, "gated to one fetch per 300s");
    }

    #[tokio::test]
    async fn expired_cache_refetches() {
        let dir = tempdir().unwrap();
        let f = Fake {
            openai: Ok(Some(10)),
            codex: None,
            calls: Cell::new(0),
        };
        let _ = self_compose(&f, dir.path(), "fp", t0(), true).await;
        let _ = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(301), true).await;
        assert_eq!(f.calls.get(), 2);
    }

    #[tokio::test]
    async fn fetch_error_serves_stale_value_marked() {
        let dir = tempdir().unwrap();
        cache::write_success(dir.path(), "fp", 999, t0());
        let f = Fake {
            openai: Err("boom".into()),
            codex: None,
            calls: Cell::new(0),
        };
        let cp = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(400), true).await;
        assert_eq!(cp.openai_cost_micro_usd, Some(999));
        assert!(cp.openai_stale, "stale value marked");
        assert_eq!(f.calls.get(), 1);
    }

    #[tokio::test]
    async fn cooldown_skips_fetch() {
        let dir = tempdir().unwrap();
        cache::write_success(dir.path(), "fp", 999, t0());
        cache::write_failure(dir.path(), "fp", t0() + Duration::seconds(400));
        let f = Fake {
            openai: Ok(Some(123)),
            codex: None,
            calls: Cell::new(0),
        };
        let cp = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(420), true).await;
        assert_eq!(f.calls.get(), 0, "in cooldown, no fetch");
        assert_eq!(cp.openai_cost_micro_usd, Some(999));
        assert!(cp.openai_stale);
    }

    #[tokio::test]
    async fn no_key_yields_no_openai_cell() {
        let dir = tempdir().unwrap();
        let f = Fake {
            openai: Ok(None),
            codex: Some(3.0),
            calls: Cell::new(0),
        };
        let cp = self_compose(&f, dir.path(), "fp", t0(), true).await;
        assert_eq!(cp.openai_cost_micro_usd, None);
        assert!(!cp.openai_stale);
        assert_eq!(cp.codex_five_hour, Some(3.0));
        // One fetch attempt is made (empty cache, no cooldown), but a missing
        // key caches nothing, so no cooldown starts.
        assert_eq!(f.calls.get(), 1);
    }

    #[tokio::test]
    async fn acquired_lease_rechecks_cache_before_fetching() {
        let dir = tempdir().unwrap();
        let lease = match cache::try_acquire_refresh_lease(dir.path()).unwrap() {
            cache::LeaseAttempt::Acquired(lease) => lease,
            cache::LeaseAttempt::Busy => panic!("lease must be available"),
        };
        cache::write_success(dir.path(), "fp", 77, t0());
        let f = Fake {
            openai: Ok(Some(88)),
            codex: None,
            calls: Cell::new(0),
        };

        let value = refresh_under_lease(&f, dir.path(), "fp", t0(), lease).await;

        assert_eq!(value, (Some(77), false));
        assert_eq!(f.calls.get(), 0, "under-lock recheck must avoid a fetch");
    }

    struct BlockingFake {
        calls: Arc<AtomicUsize>,
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    impl CrossSources for BlockingFake {
        async fn fetch_openai_total_micro_usd(&self) -> Result<Option<i64>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            self.release.notified().await;
            Ok(Some(42))
        }

        fn codex_windows(&self) -> (Option<f32>, Option<f32>) {
            (None, None)
        }
    }

    #[tokio::test]
    async fn concurrent_composers_perform_at_most_one_refresh() {
        let dir = tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let first_source = BlockingFake {
            calls: Arc::clone(&calls),
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        };
        let second_source = BlockingFake {
            calls: Arc::clone(&calls),
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        };
        let path = dir.path().to_path_buf();
        let first_path = path.clone();
        let first = tokio::spawn(async move {
            self_compose(&first_source, &first_path, "fp", t0(), true).await
        });
        started.notified().await;
        let second =
            tokio::spawn(
                async move { self_compose(&second_source, &path, "fp", t0(), true).await },
            );
        tokio::time::sleep(StdDuration::from_millis(30)).await;
        release.notify_waiters();

        let first_value = first.await.unwrap();
        let second_value = second.await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(first_value.openai_cost_micro_usd, Some(42));
        assert_eq!(second_value.openai_cost_micro_usd, Some(42));
    }

    #[tokio::test]
    async fn stale_data_returns_immediately_while_another_process_refreshes() {
        let dir = tempdir().unwrap();
        cache::write_success(dir.path(), "fp", 55, t0());
        let _lease = match cache::try_acquire_refresh_lease(dir.path()).unwrap() {
            cache::LeaseAttempt::Acquired(lease) => lease,
            cache::LeaseAttempt::Busy => panic!("lease must be available"),
        };
        let f = Fake {
            openai: Ok(Some(66)),
            codex: None,
            calls: Cell::new(0),
        };
        let started = tokio::time::Instant::now();

        let value = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(301), true).await;

        assert!(started.elapsed() < StdDuration::from_millis(100));
        assert_eq!(value.openai_cost_micro_usd, Some(55));
        assert!(value.openai_stale);
        assert_eq!(f.calls.get(), 0);
    }

    #[tokio::test]
    async fn waiting_for_another_process_is_bounded() {
        let dir = tempdir().unwrap();
        let _lease = match cache::try_acquire_refresh_lease(dir.path()).unwrap() {
            cache::LeaseAttempt::Acquired(lease) => lease,
            cache::LeaseAttempt::Busy => panic!("lease must be available"),
        };
        let f = Fake {
            openai: Ok(Some(66)),
            codex: None,
            calls: Cell::new(0),
        };
        let started = tokio::time::Instant::now();

        let value = self_compose(&f, dir.path(), "fp", t0(), true).await;

        let elapsed = started.elapsed();
        assert!(elapsed >= REFRESH_WAIT_TIMEOUT);
        assert!(elapsed < REFRESH_WAIT_TIMEOUT + StdDuration::from_millis(150));
        assert_eq!(value.openai_cost_micro_usd, None);
        assert_eq!(f.calls.get(), 0);
    }

    /// want_openai=false must not touch the cache or the network at all: no
    /// value, no staleness, and no fetch recorded on the fake. The Codex half
    /// is unaffected - it is local and cheap, and the gate is only about OpenAI.
    #[tokio::test]
    async fn want_openai_false_skips_the_fetch_entirely() {
        let dir = tempdir().unwrap();
        let f = Fake {
            openai: Ok(Some(4_200_000)),
            codex: Some(12.0),
            calls: Cell::new(0),
        };
        let cp = self_compose(&f, dir.path(), "fp", t0(), false).await;
        assert_eq!(cp.openai_cost_micro_usd, None, "no value when not wanted");
        assert!(!cp.openai_stale, "not stale, just absent");
        assert_eq!(f.calls.get(), 0, "no upstream fetch when not wanted");
        assert!(
            cache::read(dir.path(), "fp").is_none(),
            "the cache is not even touched when not wanted"
        );
        assert_eq!(cp.codex_five_hour, Some(12.0), "Codex still composed");
    }
}
