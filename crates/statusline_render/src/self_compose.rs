//! Self-compose path: build cross-provider segments without the watcher.
//!
//! The statusline calls this only when there is no fresh `snapshot.json`. It
//! reads Codex locally (cheap, every turn) and serves the OpenAI cost figure
//! through the cache (cache.rs) so the billing API is hit at most once per 300s
//! (AGENTS.md 3.1). It calls ONLY the two sources behind `CrossSources` - never
//! the Anthropic OAuth path (AGENTS.md §3.1 politeness invariant); that is why this crate
//! has no `anthropic_oauth` dependency.

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::cache;
use crate::render::CrossProvider;

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
    /// Local Codex quota percent (0..100), or `None` if Codex is absent/unparsed.
    fn codex_used_percent(&self) -> Option<f32>;
}

pub async fn self_compose<S: CrossSources>(
    sources: &S,
    cache_dir: &Path,
    fingerprint: &str,
    now: DateTime<Utc>,
) -> CrossProvider {
    // Codex: local, cheap, never cached -> current whenever present.
    let codex_used_percent = sources.codex_used_percent();

    // OpenAI: cache-gated.
    let entry = cache::read(cache_dir, fingerprint);
    let last_val = entry.as_ref().and_then(|e| e.total_micro_usd);
    let fresh = entry.as_ref().is_some_and(|e| cache::is_fresh(e, now));
    let cooled = entry.as_ref().is_some_and(|e| cache::in_cooldown(e, now));

    let (openai_cost_micro_usd, openai_stale) = if fresh {
        (last_val, false)
    } else if cooled {
        (last_val, last_val.is_some())
    } else {
        match sources.fetch_openai_total_micro_usd().await {
            Ok(Some(v)) => {
                cache::write_success(cache_dir, fingerprint, v, now);
                (Some(v), false)
            }
            Ok(None) => (None, false),
            Err(e) => {
                tracing::debug!("statusline: OpenAI self-compose fetch failed: {e}");
                cache::write_failure(cache_dir, fingerprint, now);
                (last_val, last_val.is_some())
            }
        }
    };

    CrossProvider {
        codex_used_percent,
        openai_cost_micro_usd,
        codex_stale: false,
        openai_stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache;
    use chrono::{Duration, TimeZone as _, Utc};
    use std::cell::Cell;
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
        fn codex_used_percent(&self) -> Option<f32> {
            self.codex
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
        let cp = self_compose(&f, dir.path(), "fp", t0()).await;
        assert_eq!(cp.openai_cost_micro_usd, Some(4_200_000));
        assert_eq!(cp.codex_used_percent, Some(6.0));
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
        let _ = self_compose(&f, dir.path(), "fp", t0()).await;
        let cp = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(120)).await;
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
        let _ = self_compose(&f, dir.path(), "fp", t0()).await;
        let _ = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(301)).await;
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
        let cp = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(400)).await;
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
        let cp = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(420)).await;
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
        let cp = self_compose(&f, dir.path(), "fp", t0()).await;
        assert_eq!(cp.openai_cost_micro_usd, None);
        assert!(!cp.openai_stale);
        assert_eq!(cp.codex_used_percent, Some(3.0));
        // One fetch attempt is made (empty cache, no cooldown), but a missing
        // key caches nothing, so no cooldown starts.
        assert_eq!(f.calls.get(), 1);
    }

    #[tokio::test]
    async fn seeded_cache_gates_fetch() {
        // A watcher OpenAI fetch seeded into the cache (via seed_if_newer) must
        // suppress a self-compose fetch within the TTL - the machine-wide 300s
        // gate spanning the watcher-to-statusline handoff.
        let dir = tempdir().unwrap();
        cache::seed_if_newer(dir.path(), "fp", 500, t0());
        let f = Fake {
            openai: Ok(Some(999)),
            codex: None,
            calls: Cell::new(0),
        };
        let cp = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(120)).await;
        assert_eq!(cp.openai_cost_micro_usd, Some(500));
        assert!(!cp.openai_stale);
        assert_eq!(f.calls.get(), 0, "seed within TTL gates the fetch");
    }
}
