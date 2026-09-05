//! One-shot source orchestration for CLI status. Provider gates run before I/O.
//! The live actor remains in `state_coordinator`; both paths share JSONL
//! summarization, statusline freshness, quota selection and pace derivation.
//! Source adapters own filesystem/network access; this crate owns composition.

use anthropic_oauth::ClaudeOAuthSnapshot;
use chrono::{DateTime, Utc};
use claude_parser::UsageEvent;
use claude_statusline::StatuslineFilePayload;
use codex_local::CodexQuotaSnapshot;
use openai_client::OpenAiCosts;
use settings::ProviderSettings;
use state_coordinator::{
    JsonlSnapshot, Snapshot, pace_for_snapshot_at, statusline_freshness_error, summarize_jsonl,
};
use tracing::{info, warn};

/// Source adapters for one-shot composition. Provider settings are checked
/// before invoking optional provider methods. Local Claude JSONL and statusline
/// reads remain enabled independently of the Anthropic OAuth toggle.
// Static dispatch is sufficient for the CLI and fixture adapters; no spawned
// generic future requires a Send bound on these trait methods.
#[allow(async_fn_in_trait)]
pub trait SnapshotSources {
    /// Anthropic OAuth usage. `Ok(None)` means no Claude Code credential.
    /// The impl owns read-only credential loading and rotated-bearer retry.
    async fn fetch_oauth(&self) -> anyhow::Result<Option<ClaudeOAuthSnapshot>>;
    /// Claude Code's local statusline snapshot. Missing/unwired is neutral.
    async fn load_statusline(&self) -> anyhow::Result<Option<StatuslineFilePayload>>;
    /// All deduped Claude Code JSONL events + count of files scanned.
    async fn load_claude_events(&self) -> anyhow::Result<(Vec<UsageEvent>, usize)>;
    /// Codex rate-limit snapshot. `Ok(None)` = Codex not installed (NOT an error).
    async fn fetch_codex_quota(&self) -> anyhow::Result<Option<CodexQuotaSnapshot>>;
    /// OpenAI Admin Costs. `Ok(None)` = no key configured (NOT an error).
    async fn fetch_openai(&self) -> anyhow::Result<Option<OpenAiCosts>>;
}

/// Compose one snapshot using the live provider gates and shared derivation
/// rules. The caller supplies the effective time and environment-key presence;
/// disabled providers are never entered, including their credential resolution.
pub async fn compose<S: SnapshotSources>(
    sources: &S,
    now: DateTime<Utc>,
    providers: &ProviderSettings,
    openai_env_override: bool,
) -> Snapshot {
    let (claude_statusline, claude_statusline_error) = match sources.load_statusline().await {
        Ok(payload) => {
            let error = payload
                .as_ref()
                .and_then(|p| statusline_freshness_error(p.captured_at, now));
            (payload, error)
        }
        Err(e) => (None, Some(e.to_string())),
    };
    // Gate before entering the I/O adapter, including credential resolution.
    let oauth = if providers.anthropic_enabled {
        sources.fetch_oauth().await
    } else {
        Ok(None)
    };
    let (claude_oauth, claude_oauth_error, claude_oauth_unavailable) = match oauth {
        Ok(Some(s)) => (Some(s), None, None),
        Ok(None) => (
            None,
            None,
            providers
                .anthropic_enabled
                .then(|| "Claude Code not detected".to_string()),
        ),
        Err(e) => {
            warn!("OAuth source failed: {e}");
            (None, Some(e.to_string()), None)
        }
    };

    // Anchor the JSONL rolling window to Anthropic's authoritative 5-hour
    // reset when we have it (removes local clock-drift error); fall back to
    // now-relative when OAuth is unavailable. AGENTS.md §7.
    let window_anchor = claude_oauth
        .as_ref()
        .and_then(ClaudeOAuthSnapshot::five_hour_reset);

    // JSONL events power BOTH the window summary and the API-rate cost
    // synthesis. Read once, summarize twice. If the load fails entirely,
    // both downstream slots stay None and only claude_jsonl_error carries
    // the reason - we don't duplicate it into anthropic_api_cost_error.
    let mut claude_jsonl: Option<JsonlSnapshot> = None;
    let mut claude_jsonl_error: Option<String> = None;
    let mut anthropic_api_cost: Option<claude_cost::Cost> = None;
    let mut anthropic_api_cost_error: Option<String> = None;
    match sources.load_claude_events().await {
        Ok((events, files_scanned)) => {
            // Load the bundled LiteLLM price table; a load failure degrades only
            // the cost cell (the window is still produced). `summarize_jsonl` is
            // the SAME pipeline the live coordinator runs, so the one-shot CLI
            // and the watcher cannot diverge (AGENTS.md §4 #8).
            let prices = match claude_cost::load_bundled_prices() {
                Ok(p) => Some(p),
                Err(e) => {
                    warn!("claude_cost: bundled price table failed to load: {e}");
                    None
                }
            };
            let cells =
                summarize_jsonl(&events, now, files_scanned, window_anchor, prices.as_ref());
            claude_jsonl = Some(cells.jsonl);
            match cells.cost {
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
                    anthropic_api_cost_error = Some(e);
                }
            }
        }
        Err(e)
            if matches!(
                e.downcast_ref::<claude_parser::ParseError>(),
                Some(claude_parser::ParseError::FileMissing(_))
            ) => {}
        Err(e) => {
            warn!("JSONL source failed: {e}");
            claude_jsonl_error = Some(e.to_string());
        }
    }

    let codex = if providers.codex_enabled {
        sources.fetch_codex_quota().await
    } else {
        Ok(None)
    };
    let (codex_quota, codex_quota_error) = match codex {
        Ok(snap) => (snap, None),
        Err(e) => {
            warn!("codex_quota source failed: {e}");
            (None, Some(e.to_string()))
        }
    };

    let costs = if providers.openai_enabled || openai_env_override {
        sources.fetch_openai().await
    } else {
        Ok(None)
    };
    let (openai, openai_error) = match costs {
        Ok(Some(g)) => (Some(g), None),
        Ok(None) => (None, None),
        Err(e) => {
            warn!("OpenAI source failed: {e}");
            (None, Some(e.to_string()))
        }
    };

    let mut snapshot = Snapshot {
        schema_version: state_coordinator::SNAPSHOT_SCHEMA_VERSION,
        fetched_at: now,
        claude_oauth,
        claude_oauth_error,
        claude_oauth_unavailable,
        claude_jsonl,
        claude_jsonl_error,
        anthropic_api_cost,
        anthropic_api_cost_error,
        codex_quota,
        codex_quota_error,
        openai,
        openai_error,
        claude_statusline,
        claude_statusline_error,
        pace: Vec::new(),
    };
    snapshot.pace = pace_for_snapshot_at(&snapshot, now);
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 16, 12, 0, 0).unwrap()
    }

    fn all_providers() -> ProviderSettings {
        ProviderSettings {
            anthropic_enabled: true,
            codex_enabled: true,
            openai_enabled: true,
        }
    }

    #[derive(Default)]
    struct Fake {
        oauth: Option<anyhow::Result<ClaudeOAuthSnapshot>>,
        statusline: Option<anyhow::Result<Option<StatuslineFilePayload>>>,
        calls: std::cell::RefCell<Vec<&'static str>>,
        events: Option<anyhow::Result<(Vec<UsageEvent>, usize)>>,
        codex: Option<anyhow::Result<Option<CodexQuotaSnapshot>>>,
        openai: Option<anyhow::Result<Option<OpenAiCosts>>>,
    }
    impl SnapshotSources for Fake {
        async fn fetch_oauth(&self) -> anyhow::Result<Option<ClaudeOAuthSnapshot>> {
            self.calls.borrow_mut().push("oauth");
            match &self.oauth {
                Some(Ok(s)) => Ok(Some(s.clone())),
                Some(Err(e)) => Err(anyhow::anyhow!("{e}")),
                None => Ok(None),
            }
        }
        async fn load_statusline(&self) -> anyhow::Result<Option<StatuslineFilePayload>> {
            self.calls.borrow_mut().push("statusline");
            match &self.statusline {
                Some(Ok(payload)) => Ok(payload.clone()),
                Some(Err(error)) => Err(anyhow::anyhow!("{error}")),
                None => Ok(None),
            }
        }
        async fn load_claude_events(&self) -> anyhow::Result<(Vec<UsageEvent>, usize)> {
            self.calls.borrow_mut().push("jsonl");
            match &self.events {
                Some(Ok(v)) => Ok(v.clone()),
                // Preserve the classification the real filesystem adapter supplies.
                Some(Err(e))
                    if matches!(
                        e.downcast_ref::<claude_parser::ParseError>(),
                        Some(claude_parser::ParseError::FileMissing(_))
                    ) =>
                {
                    Err(claude_parser::ParseError::FileMissing("absent-projects".into()).into())
                }
                Some(Err(e)) => Err(anyhow::anyhow!("{e}")),
                None => Ok((Vec::new(), 0)),
            }
        }
        async fn fetch_codex_quota(&self) -> anyhow::Result<Option<CodexQuotaSnapshot>> {
            self.calls.borrow_mut().push("codex");
            match &self.codex {
                Some(Ok(v)) => Ok(v.clone()),
                Some(Err(e)) => Err(anyhow::anyhow!("{e}")),
                None => Ok(None),
            }
        }
        async fn fetch_openai(&self) -> anyhow::Result<Option<OpenAiCosts>> {
            self.calls.borrow_mut().push("openai");
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

    fn quota_sources(captured_at: DateTime<Utc>, key: &str) -> Fake {
        let n = now();
        Fake {
            oauth: Some(Ok(ClaudeOAuthSnapshot {
                cadences: vec![anthropic_oauth::CadenceBar {
                    key: "five_hour".to_string(),
                    display_label: "5-hour".to_string(),
                    utilization_percent: 80.0,
                    resets_at: n + chrono::Duration::hours(2),
                }],
                extra_usage: Some(anthropic_oauth::ExtraUsage {
                    is_enabled: true,
                    used_credits_micro_usd: 2_000_000,
                    monthly_limit_micro_usd: 10_000_000,
                    utilization_percent: 20.0,
                    currency: "USD".to_string(),
                }),
                subscription_type: None,
                rate_limit_tier: None,
                org_uuid: None,
                fetched_at: n,
            })),
            statusline: Some(Ok(Some(StatuslineFilePayload::new(
                claude_statusline::StatuslineSnapshot {
                    rate_limits: Some(claude_statusline::RateLimits {
                        windows: vec![claude_statusline::RateWindow {
                            key: key.to_string(),
                            label: key.to_string(),
                            used_percent: 20.0,
                            resets_at: n + chrono::Duration::hours(3),
                        }],
                    }),
                    session_cost_micro_usd: None,
                    claude_code_version: None,
                    model_display_name: None,
                    context_used_percent: None,
                },
                captured_at,
            )))),
            events: Some(Ok((vec![one_event(n)], 1))),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn disabled_sources_are_never_called_but_local_claude_remains_available() {
        for mask in 0..8 {
            for env_override in [false, true] {
                let providers = ProviderSettings {
                    anthropic_enabled: mask & 1 != 0,
                    codex_enabled: mask & 2 != 0,
                    openai_enabled: mask & 4 != 0,
                };
                let f = quota_sources(now(), "five_hour");
                let snap = compose(&f, now(), &providers, env_override).await;
                let calls = f.calls.borrow();
                assert_eq!(calls.contains(&"oauth"), providers.anthropic_enabled);
                assert_eq!(calls.contains(&"codex"), providers.codex_enabled);
                assert_eq!(
                    calls.contains(&"openai"),
                    providers.openai_enabled || env_override
                );
                assert!(calls.contains(&"jsonl") && calls.contains(&"statusline"));
                assert!(snap.claude_statusline.is_some() && snap.claude_jsonl.is_some());
                assert_eq!(snap.claude_oauth.is_some(), providers.anthropic_enabled);
                assert!(snap.claude_oauth_unavailable.is_none());
            }
        }
    }

    #[tokio::test]
    async fn one_shot_quota_and_pace_use_the_live_freshness_boundaries() {
        for (age_secs, fresh) in [(-1, false), (0, true), (900, true), (901, false)] {
            let f = quota_sources(now() - chrono::Duration::seconds(age_secs), "five_hour");
            let snap = compose(&f, now(), &all_providers(), false).await;
            assert_eq!(snap.statusline_fresh(), fresh, "age={age_secs}");
            assert_eq!(snap.claude_statusline_error.is_none(), fresh);
            assert!(snap.claude_statusline.is_some(), "retain stale payloads");
            assert_eq!(
                matches!(
                    snap.anthropic_quota_source(),
                    Some(state_coordinator::AnthropicQuotaSource::Statusline { .. })
                ),
                fresh
            );
            assert_eq!(snap.pace.len(), 1);
            assert_eq!(snap.pace[0].used_fraction, if fresh { 0.2 } else { 0.8 });
            assert_eq!(
                snap.claude_oauth
                    .as_ref()
                    .unwrap()
                    .extra_usage
                    .as_ref()
                    .unwrap()
                    .used_credits_micro_usd,
                2_000_000
            );
        }
    }

    #[tokio::test]
    async fn missing_and_failed_sources_remain_distinct_with_statusline_fallback() {
        let snap = compose(&Fake::default(), now(), &all_providers(), false).await;
        assert_eq!(
            snap.claude_oauth_unavailable.as_deref(),
            Some("Claude Code not detected")
        );
        assert!(snap.claude_oauth_error.is_none() && snap.claude_statusline_error.is_none());

        let mut f = quota_sources(now(), "five_hour");
        f.oauth = Some(Err(anyhow::anyhow!("credential expired")));
        let snap = compose(&f, now(), &all_providers(), false).await;
        assert_eq!(
            snap.claude_oauth_error.as_deref(),
            Some("credential expired")
        );
        assert!(snap.claude_oauth_unavailable.is_none());
        assert_eq!(snap.pace[0].used_fraction, 0.2);

        let mut f = quota_sources(now(), "five_hour");
        f.statusline = Some(Err(anyhow::anyhow!("statusline read failed")));
        let snap = compose(&f, now(), &all_providers(), false).await;
        assert_eq!(
            snap.claude_statusline_error.as_deref(),
            Some("statusline read failed")
        );
        assert_eq!(snap.pace[0].used_fraction, 0.8);
    }

    #[tokio::test]
    async fn unknown_statusline_family_does_not_mix_pace_sources() {
        let f = quota_sources(now(), "monthly");
        let snap = compose(&f, now(), &all_providers(), false).await;
        assert!(matches!(
            snap.anthropic_quota_source(),
            Some(state_coordinator::AnthropicQuotaSource::Statusline { .. })
        ));
        assert_eq!(snap.pace.len(), 1);
        assert_eq!(snap.pace[0].key, "five_hour");
        assert_eq!(snap.pace[0].used_fraction, 0.8);
    }

    #[tokio::test]
    async fn one_shot_matches_live_actor_for_fresh_stale_and_unknown_statusline() {
        use state_coordinator::{
            ClaudeJsonlInput, NullSink, SourcePartial, SourceUpdate, StateMsg,
        };
        for (age, key) in [
            (0, "five_hour"),
            (960, "five_hour"),
            (-60, "five_hour"),
            (0, "monthly"),
        ] {
            let base = chrono::Utc::now();
            let mut f = quota_sources(base - chrono::Duration::seconds(age), key);
            let oauth = f.oauth.as_mut().unwrap().as_mut().unwrap();
            oauth.fetched_at = base;
            oauth.cadences[0].resets_at = base + chrono::Duration::hours(2);
            f.events = Some(Ok((vec![one_event(base)], 1)));
            let payload = f
                .statusline
                .as_mut()
                .unwrap()
                .as_mut()
                .unwrap()
                .as_mut()
                .unwrap();
            payload.payload.rate_limits.as_mut().unwrap().windows[0].resets_at =
                base + chrono::Duration::hours(3);

            let (handle, task) = state_coordinator::spawn(NullSink);
            let settings = settings::Settings {
                providers: all_providers(),
                ..Default::default()
            };
            handle.transition_settings(settings, 1).await.unwrap();
            let partials = [
                SourcePartial::ClaudeOAuth(f.oauth.as_ref().unwrap().as_ref().unwrap().clone()),
                SourcePartial::ClaudeJsonl(ClaudeJsonlInput {
                    events: std::sync::Arc::new(
                        f.events.as_ref().unwrap().as_ref().unwrap().0.clone(),
                    ),
                    files_scanned: 1,
                }),
                SourcePartial::ClaudeStatusline(
                    f.statusline
                        .as_ref()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .clone(),
                ),
            ];
            for partial in partials {
                handle
                    .send(StateMsg::Update(SourceUpdate {
                        generation: 1,
                        source: partial.source(),
                        result: Ok(partial),
                    }))
                    .await
                    .unwrap();
            }
            let live = handle.query().await.unwrap();
            let once = compose(&f, live.fetched_at, &all_providers(), false).await;
            assert_eq!(once.pace, live.pace, "age={age}, key={key}");
            assert_eq!(once.claude_statusline_error, live.claude_statusline_error);
            assert_eq!(
                once.claude_jsonl.as_ref().unwrap().window,
                live.claude_jsonl.as_ref().unwrap().window
            );
            assert_eq!(
                once.claude_jsonl
                    .as_ref()
                    .unwrap()
                    .window
                    .total_events_in_window,
                1
            );
            assert_eq!(
                once.anthropic_api_cost.as_ref().unwrap().total_micro_usd,
                live.anthropic_api_cost.as_ref().unwrap().total_micro_usd
            );
            assert!(once.anthropic_api_cost.as_ref().unwrap().total_micro_usd > 0);
            assert_eq!(once.statusline_fresh(), live.statusline_fresh());
            assert!(once.claude_oauth_error.is_none() && live.claude_oauth_error.is_none());
            drop(handle);
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn jsonl_error_keeps_both_anthropic_cells_none_with_single_error() {
        let f = Fake {
            events: Some(Err(anyhow::anyhow!("permission denied"))),
            ..Default::default()
        };
        let snap = compose(&f, now(), &all_providers(), false).await;
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
    async fn missing_jsonl_directory_is_neutral_like_the_waiting_live_watcher() {
        let f = Fake {
            events: Some(Err(claude_parser::ParseError::FileMissing(
                "absent-projects".into(),
            )
            .into())),
            ..Default::default()
        };
        let snap = compose(&f, now(), &all_providers(), false).await;
        assert!(snap.claude_jsonl.is_none() && snap.claude_jsonl_error.is_none());
        assert!(snap.anthropic_api_cost.is_none() && snap.anthropic_api_cost_error.is_none());
    }

    #[tokio::test]
    async fn codex_and_openai_none_set_no_error() {
        let f = Fake {
            events: Some(Ok((vec![one_event(now())], 1))),
            ..Default::default()
        };
        let snap = compose(&f, now(), &all_providers(), false).await;
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
        let snap = compose(&f, now(), &all_providers(), false).await;
        assert_eq!(snap.claude_oauth_error.as_deref(), Some("AuthExpired"));
        let w = snap.claude_jsonl.unwrap().window;
        assert_eq!(
            w.window_start,
            now() - window::DEFAULT_WINDOW,
            "no oauth ⇒ now-relative window"
        );
    }

    #[tokio::test]
    async fn codex_error_sets_only_its_own_error_slot() {
        // A codex_local failure must populate codex_quota_error and leave
        // codex_quota None, WITHOUT touching any other cell - the
        // Anthropic cells still come from a successful JSONL load, and
        // OpenAI stays at its "not configured" Ok(None) baseline.
        let f = Fake {
            events: Some(Ok((vec![one_event(now())], 1))),
            codex: Some(Err(anyhow::anyhow!("permission denied on ~/.codex"))),
            ..Default::default()
        };
        let snap = compose(&f, now(), &all_providers(), false).await;

        assert!(snap.codex_quota.is_none());
        assert_eq!(
            snap.codex_quota_error.as_deref(),
            Some("permission denied on ~/.codex")
        );

        // No cross-contamination into the other three sources.
        assert!(snap.claude_jsonl.is_some());
        assert_eq!(snap.claude_jsonl_error, None);
        assert!(snap.anthropic_api_cost.is_some());
        assert_eq!(snap.anthropic_api_cost_error, None);
        assert!(snap.openai.is_none());
        assert_eq!(
            snap.openai_error, None,
            "openai Ok(None) baseline must NOT acquire an error from a codex failure"
        );
    }

    #[tokio::test]
    async fn openai_error_sets_only_its_own_error_slot() {
        // Symmetric to the codex case: an OpenAI Admin Costs failure
        // populates openai_error only, leaves openai None, and never
        // bleeds into any other cell.
        let f = Fake {
            events: Some(Ok((vec![one_event(now())], 1))),
            openai: Some(Err(anyhow::anyhow!("HTTP 403 - admin scope required"))),
            ..Default::default()
        };
        let snap = compose(&f, now(), &all_providers(), false).await;

        assert!(snap.openai.is_none());
        assert_eq!(
            snap.openai_error.as_deref(),
            Some("HTTP 403 - admin scope required")
        );

        // No cross-contamination.
        assert!(snap.claude_jsonl.is_some());
        assert_eq!(snap.claude_jsonl_error, None);
        assert!(snap.anthropic_api_cost.is_some());
        assert_eq!(snap.anthropic_api_cost_error, None);
        assert!(snap.codex_quota.is_none());
        assert_eq!(
            snap.codex_quota_error, None,
            "codex Ok(None) baseline must NOT acquire an error from an openai failure"
        );
    }

    #[tokio::test]
    async fn oauth_present_anchors_window_to_five_hour_reset() {
        use anthropic_oauth::CadenceBar;
        let n = now();
        let reset = n + chrono::Duration::hours(2); // anchored window [reset-5h, reset)
        let oauth = ClaudeOAuthSnapshot {
            cadences: vec![CadenceBar {
                key: "five_hour".to_string(),
                display_label: "Current 5-hour session".to_string(),
                utilization_percent: 42.0,
                resets_at: reset,
            }],
            extra_usage: None,
            subscription_type: None,
            rate_limit_tier: None,
            org_uuid: None,
            fetched_at: n,
        };
        let f = Fake {
            oauth: Some(Ok(oauth)),
            events: Some(Ok((vec![one_event(n)], 1))),
            ..Default::default()
        };
        let snap = compose(&f, n, &all_providers(), false).await;
        assert!(snap.claude_oauth.is_some());
        assert!(snap.claude_oauth_error.is_none());
        let w = snap.claude_jsonl.expect("jsonl populated").window;
        assert_eq!(
            w.window_start,
            reset - window::DEFAULT_WINDOW,
            "a present five_hour reset must anchor the window (NOT now - 5h)"
        );
        assert!(snap.anthropic_api_cost.is_some());
        assert!(snap.anthropic_api_cost_error.is_none());
    }
}
