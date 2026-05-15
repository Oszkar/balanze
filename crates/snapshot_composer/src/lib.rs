//! Shared source-orchestration policy. This is the SINGLE composition path
//! (AGENTS.md §4 #8): `balanze_cli` runs it via `LiveSources`, the future
//! watcher will run it via its own `SnapshotSources` impl, and the
//! integration test runs it via `FixtureSources` — so the policy cannot
//! silently diverge between entry-points. Pure orchestration: it does no
//! network/filesystem I/O itself (that is the `SnapshotSources` impl's job)
//! and never imports `reqwest`.

use anthropic_oauth::ClaudeOAuthSnapshot;
use chrono::{DateTime, Utc};
use claude_parser::UsageEvent;
use codex_local::CodexQuotaSnapshot;
use openai_client::OpenAiCosts;
use state_coordinator::{JsonlSnapshot, Snapshot};
use tracing::{info, warn};
use window::{summarize_window, DEFAULT_BURN_WINDOW, DEFAULT_MIN_BURN_EVENTS, DEFAULT_WINDOW};

/// The four I/O-bound source fetches `compose` needs. CLI (`LiveSources`),
/// the future watcher, and tests (`FixtureSources`) provide impls. The trait
/// sits at the I/O boundary; the pure transforms (cost synthesis, window
/// math) live in `compose` so the orchestration policy is testable without
/// network/filesystem and is identical across entry-points.
///
/// `async fn` in a trait is stable since Rust 1.75 (MSRV here is 1.77). We
/// only ever use STATIC dispatch (`compose<S: SnapshotSources>`), never
/// `dyn SnapshotSources`, so the `async_fn_in_trait` lint's Send-bound
/// caveat does not apply — hence the documented allow.
#[allow(async_fn_in_trait)]
pub trait SnapshotSources {
    /// Anthropic OAuth usage. The impl owns credential load + proactive
    /// refresh + 401-retry (OAuth-fetch detail, not composition policy).
    async fn fetch_oauth(&self) -> anyhow::Result<ClaudeOAuthSnapshot>;
    /// All deduped Claude Code JSONL events + count of files scanned.
    async fn load_claude_events(&self) -> anyhow::Result<(Vec<UsageEvent>, usize)>;
    /// Codex rate-limit snapshot. `Ok(None)` = Codex not installed (NOT an error).
    async fn fetch_codex_quota(&self) -> anyhow::Result<Option<CodexQuotaSnapshot>>;
    /// OpenAI Admin Costs. `Ok(None)` = no key configured (NOT an error).
    async fn fetch_openai(&self) -> anyhow::Result<Option<OpenAiCosts>>;
}

/// Compose one `Snapshot` from the four sources, applying the exact
/// per-source error-mapping policy (AGENTS.md §4 #8). Moved verbatim from
/// the former `balanze_cli::build_snapshot`; behavior is unchanged.
pub async fn compose<S: SnapshotSources>(sources: &S, now: DateTime<Utc>) -> Snapshot {
    let (claude_oauth, claude_oauth_error) = match sources.fetch_oauth().await {
        Ok(s) => (Some(s), None),
        Err(e) => {
            warn!("OAuth source failed: {e}");
            (None, Some(e.to_string()))
        }
    };

    // Anchor the JSONL rolling window to Anthropic's authoritative 5-hour
    // reset when we have it (removes local clock-drift error); fall back to
    // now-relative when OAuth is unavailable. AGENTS.md v0.1.1 / §7.
    let window_anchor = claude_oauth
        .as_ref()
        .and_then(ClaudeOAuthSnapshot::five_hour_reset);

    // JSONL events power BOTH the window summary and the API-rate cost
    // synthesis. Read once, summarize twice. If the load fails entirely,
    // both downstream slots stay None and only claude_jsonl_error carries
    // the reason — we don't duplicate it into anthropic_api_cost_error.
    let mut claude_jsonl: Option<JsonlSnapshot> = None;
    let mut claude_jsonl_error: Option<String> = None;
    let mut anthropic_api_cost: Option<claude_cost::Cost> = None;
    let mut anthropic_api_cost_error: Option<String> = None;
    match sources.load_claude_events().await {
        Ok((events, files_scanned)) => {
            let window = summarize_window(
                &events,
                now,
                DEFAULT_WINDOW,
                DEFAULT_BURN_WINDOW,
                DEFAULT_MIN_BURN_EVENTS,
                window_anchor,
            );
            claude_jsonl = Some(JsonlSnapshot {
                files_scanned,
                window,
            });
            match compute_anthropic_api_cost(&events) {
                Ok(cost) => {
                    info!(
                        "claude_cost: total_micro_usd={} per_model_rows={} skipped={}",
                        cost.total_micro_usd,
                        cost.per_model.len(),
                        cost.skipped_models.len()
                    );
                    anthropic_api_cost = Some(cost);
                }
                Err(e) => {
                    warn!("anthropic_api_cost source failed: {e}");
                    anthropic_api_cost_error = Some(e.to_string());
                }
            }
        }
        Err(e) => {
            warn!("JSONL source failed: {e}");
            claude_jsonl_error = Some(e.to_string());
        }
    }

    let (codex_quota, codex_quota_error) = match sources.fetch_codex_quota().await {
        Ok(snap) => (snap, None),
        Err(e) => {
            warn!("codex_quota source failed: {e}");
            (None, Some(e.to_string()))
        }
    };

    let (openai, openai_error) = match sources.fetch_openai().await {
        Ok(Some(g)) => (Some(g), None),
        Ok(None) => (None, None),
        Err(e) => {
            warn!("OpenAI source failed: {e}");
            (None, Some(e.to_string()))
        }
    };

    Snapshot {
        fetched_at: now,
        claude_oauth,
        claude_oauth_error,
        claude_jsonl,
        claude_jsonl_error,
        anthropic_api_cost,
        anthropic_api_cost_error,
        codex_quota,
        codex_quota_error,
        openai,
        openai_error,
    }
}

/// Synthesize the API-rate cost from the JSONL events. Pure (no I/O);
/// moved verbatim from `balanze_cli::compute_anthropic_api_cost`.
fn compute_anthropic_api_cost(events: &[UsageEvent]) -> anyhow::Result<claude_cost::Cost> {
    let prices = claude_cost::load_bundled_prices()
        .map_err(|e| anyhow::anyhow!("claude_cost: bundled price table failed to load: {e}"))?;
    Ok(claude_cost::compute_cost(events, &prices))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap()
    }

    #[derive(Default)]
    struct Fake {
        oauth: Option<anyhow::Result<ClaudeOAuthSnapshot>>,
        events: Option<anyhow::Result<(Vec<UsageEvent>, usize)>>,
        codex: Option<anyhow::Result<Option<CodexQuotaSnapshot>>>,
        openai: Option<anyhow::Result<Option<OpenAiCosts>>>,
    }
    impl SnapshotSources for Fake {
        async fn fetch_oauth(&self) -> anyhow::Result<ClaudeOAuthSnapshot> {
            match &self.oauth {
                Some(Ok(s)) => Ok(s.clone()),
                Some(Err(e)) => Err(anyhow::anyhow!("{e}")),
                None => Err(anyhow::anyhow!("oauth not configured in fake")),
            }
        }
        async fn load_claude_events(&self) -> anyhow::Result<(Vec<UsageEvent>, usize)> {
            match &self.events {
                Some(Ok(v)) => Ok(v.clone()),
                Some(Err(e)) => Err(anyhow::anyhow!("{e}")),
                None => Ok((Vec::new(), 0)),
            }
        }
        async fn fetch_codex_quota(&self) -> anyhow::Result<Option<CodexQuotaSnapshot>> {
            match &self.codex {
                Some(Ok(v)) => Ok(v.clone()),
                Some(Err(e)) => Err(anyhow::anyhow!("{e}")),
                None => Ok(None),
            }
        }
        async fn fetch_openai(&self) -> anyhow::Result<Option<OpenAiCosts>> {
            match &self.openai {
                Some(Ok(v)) => Ok(v.clone()),
                Some(Err(e)) => Err(anyhow::anyhow!("{e}")),
                None => Ok(None),
            }
        }
    }

    fn one_event(now: DateTime<Utc>) -> UsageEvent {
        use claude_parser::{AccountType, DataSource, Provider};
        UsageEvent {
            ts: now - chrono::Duration::minutes(10),
            provider: Provider::Claude,
            account_type: AccountType::Subscription,
            model: "claude-sonnet-4-6".to_string(),
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_micro_usd: None,
            source: DataSource::Jsonl,
            message_id: None,
            request_id: None,
        }
    }

    #[tokio::test]
    async fn jsonl_error_keeps_both_anthropic_cells_none_with_single_error() {
        let f = Fake {
            events: Some(Err(anyhow::anyhow!("permission denied"))),
            ..Default::default()
        };
        let snap = compose(&f, now()).await;
        assert!(snap.claude_jsonl.is_none());
        assert_eq!(
            snap.claude_jsonl_error.as_deref(),
            Some("permission denied")
        );
        assert!(snap.anthropic_api_cost.is_none());
        assert!(
            snap.anthropic_api_cost_error.is_none(),
            "JSONL error must NOT duplicate into cost cell"
        );
    }

    #[tokio::test]
    async fn codex_and_openai_none_set_no_error() {
        let f = Fake {
            events: Some(Ok((vec![one_event(now())], 1))),
            ..Default::default()
        };
        let snap = compose(&f, now()).await;
        assert!(snap.codex_quota.is_none() && snap.codex_quota_error.is_none());
        assert!(snap.openai.is_none() && snap.openai_error.is_none());
        assert!(snap.claude_jsonl.is_some());
        assert!(snap.anthropic_api_cost.is_some());
    }

    #[tokio::test]
    async fn oauth_error_falls_back_to_now_relative_window() {
        let f = Fake {
            oauth: Some(Err(anyhow::anyhow!("AuthExpired"))),
            events: Some(Ok((vec![one_event(now())], 1))),
            ..Default::default()
        };
        let snap = compose(&f, now()).await;
        assert_eq!(snap.claude_oauth_error.as_deref(), Some("AuthExpired"));
        let w = snap.claude_jsonl.unwrap().window;
        assert_eq!(
            w.window_start,
            now() - window::DEFAULT_WINDOW,
            "no oauth ⇒ now-relative window"
        );
    }
}
