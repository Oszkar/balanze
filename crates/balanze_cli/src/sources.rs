//! The production `SnapshotSources`: real network + filesystem + keychain.
//! `build_snapshot` runs `snapshot_composer::compose` over `LiveSources`; the
//! per-source helpers are the I/O adapters (AGENTS.md §4 #8 - glue, not logic).

use anyhow::{Result, anyhow};
use chrono::Utc;

use anthropic_oauth::{
    ClaudeOAuthSnapshot, CredentialsClaudeAiOauth, DEFAULT_API_BASE as ANTHROPIC_API_BASE,
    OAuthError, fetch_usage, load_from_source, locate_credentials,
};
use claude_parser::{
    IncrementalParser, UsageEvent, dedup_events, find_all_claude_projects_dirs,
    find_claude_projects_dir, find_jsonl_files,
};
use openai_client::{OpenAiCosts, gated_costs_this_month};
use state_coordinator::Snapshot;
use tracing::{info, warn};

// One-shot status loads provider settings once, then shares source gates and
// derivation policy with the live path without requiring a running tray host.
pub(crate) async fn build_snapshot() -> Snapshot {
    let settings = tokio::task::spawn_blocking(settings::load_or_default)
        .await
        .unwrap_or_else(|error| {
            warn!("settings load task failed: {error}; using defaults");
            settings::Settings::default()
        });
    let openai_env_override =
        std::env::var("BALANZE_OPENAI_KEY").is_ok_and(|v| !v.trim().is_empty());
    snapshot_composer::compose(
        &LiveSources,
        Utc::now(),
        &settings.providers,
        openai_env_override,
    )
    .await
}

/// `export` reuses the exact JSONL walk + dedup `status` uses (DRY): one source
/// of truth for which roots are scanned and how events are deduped.
pub(crate) fn export_load_claude_events() -> Result<(Vec<UsageEvent>, usize)> {
    live_load_claude_events()
}

/// `export` reuses the exact OpenAI fetch `status` uses, including the
/// `BALANZE_OPENAI_KEY` env precedence over the keychain (AGENTS.md §3.4).
pub(crate) async fn export_fetch_openai() -> Result<Option<OpenAiCosts>> {
    live_fetch_openai().await
}

/// The production `SnapshotSources`: real network + filesystem + keychain.
/// Every method body delegates to the pre-extraction helper, moved unchanged.
struct LiveSources;

impl snapshot_composer::SnapshotSources for LiveSources {
    async fn fetch_oauth(&self) -> Result<Option<ClaudeOAuthSnapshot>> {
        live_fetch_oauth().await
    }
    async fn load_statusline(&self) -> Result<Option<claude_statusline::StatuslineFilePayload>> {
        tokio::task::spawn_blocking(|| {
            let Some(path) = settings::statusline_snapshot_path() else {
                return Ok(None);
            };
            match claude_statusline::read_snapshot(&path) {
                Ok(payload) => Ok(Some(payload)),
                Err(claude_statusline::FileIoError::FileMissing { .. }) => Ok(None),
                Err(error) => Err(error.into()),
            }
        })
        .await?
    }
    async fn load_claude_events(&self) -> Result<(Vec<UsageEvent>, usize)> {
        // Sync filesystem walk + parse; keep it off the runtime worker, mirroring
        // fetch_oauth below (AGENTS.md §2.1 - never block the async runtime).
        tokio::task::spawn_blocking(live_load_claude_events).await?
    }
    async fn fetch_codex_quota(&self) -> Result<Option<codex_local::CodexQuotaSnapshot>> {
        tokio::task::spawn_blocking(live_fetch_codex_quota).await?
    }
    async fn fetch_openai(&self) -> Result<Option<OpenAiCosts>> {
        live_fetch_openai().await
    }
}

/// Load + dedup all UsageEvents from `~/.claude/projects/`. Shared input
/// for both the window summary and the claude_cost synthesis - we don't
/// want to walk + parse 491 JSONL files twice per `balanze-cli` invocation.
///
/// Returns `(events, files_scanned)`. Unreadable files and malformed complete
/// records are logged at WARN; valid complete records remain usable.
fn live_load_claude_events() -> Result<(Vec<UsageEvent>, usize)> {
    // Union ALL existing project roots: a dual-install machine can have both
    // ~/.claude/projects and ~/.config/claude/projects, and reading only the
    // first silently undercounts events + cost. `dedup_events` below collapses
    // any session that appears under more than one root.
    let roots = find_all_claude_projects_dirs();
    if roots.is_empty() {
        // No projects dir anywhere - surface the canonical FileMissing error
        // (compose maps it to claude_jsonl_error), preserving the prior
        // single-root "JSONL source failed" behavior rather than an empty-Ok.
        find_claude_projects_dir()?;
    }

    load_claude_events_from_roots(&roots)
}

fn load_claude_events_from_roots(roots: &[std::path::PathBuf]) -> Result<(Vec<UsageEvent>, usize)> {
    let mut files = Vec::new();
    let mut walk_err = None;
    for root in roots {
        match find_jsonl_files(root) {
            Ok(mut f) => files.append(&mut f),
            Err(e) => {
                warn!("jsonl: skipping root {} ({e})", root.display());
                walk_err.get_or_insert(e);
            }
        }
    }
    // No files collected from ANY root AND at least one root failed to walk
    // (e.g. permission denied) ⇒ surface that error rather than reporting an
    // empty window that may be wrong - the unreadable root could hold events.
    // (This also fires when another root walked successfully but was empty:
    // an unreadable root must not masquerade as an empty-but-fine result.)
    // A partial success - ≥1 file found on any root - keeps what walked and
    // only warns about the failed roots, above.
    if files.is_empty()
        && let Some(e) = walk_err
    {
        return Err(e.into());
    }
    info!(
        "jsonl: scanning {} files across {} root(s)",
        files.len(),
        roots.len()
    );

    let mut all_events: Vec<UsageEvent> = Vec::new();
    // A fresh reader scans each file once but shares the live path's complete-
    // record, UTF-8 and corruption handling. Nothing is persisted between runs.
    let mut parser = IncrementalParser::new();
    for path in &files {
        match parser.read_incremental(path) {
            Ok(read) => all_events.extend(read.events().iter().cloned()),
            Err(e) => warn!("jsonl: skipping {} ({e})", path.display()),
        }
    }

    let before = all_events.len();
    dedup_events(&mut all_events);
    let after = all_events.len();
    if before != after {
        info!(
            "jsonl: deduped {} → {} events ({} duplicates collapsed by (msg_id, req_id))",
            before,
            after,
            before - after
        );
    }

    Ok((all_events, files.len()))
}

/// Read the latest Codex rate-limit snapshot. Treats "Codex not installed"
/// as `Ok(None)` (not a failure - just an unconfigured source); only
/// surfaces actual errors (permission denied, schema drift, etc.).
fn live_fetch_codex_quota() -> Result<Option<codex_local::CodexQuotaSnapshot>> {
    match codex_local::read_codex_quota() {
        Ok(snap) => {
            if let Some(ref s) = snap {
                info!(
                    "codex_quota: used_percent={} plan_type={} rate_limit_reached={}",
                    s.primary.used_percent, s.plan_type, s.rate_limit_reached
                );
            } else {
                info!("codex_quota: no session data yet");
            }
            Ok(snap)
        }
        Err(codex_local::ParseError::FileMissing(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

async fn live_fetch_oauth() -> Result<Option<ClaudeOAuthSnapshot>> {
    // locate+load is sync I/O (a file read, or a `security` subprocess on
    // macOS that can block on a Keychain access prompt), so run it on a
    // blocking worker rather than stalling a tokio runtime thread (AGENTS.md
    // §2.1).
    let initial = tokio::task::spawn_blocking(|| {
        let source = locate_credentials()?;
        load_from_source(&source)
    })
    .await?;
    // Only absence on the initial probe is neutral, matching watcher startup.
    // A credential disappearing during a 401 re-read is still a poll failure.
    let creds = match initial {
        Ok(creds) => creds,
        Err(OAuthError::CredentialsMissing { .. }) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let oauth = creds.claude_ai_oauth;
    let client = reqwest::Client::builder()
        .user_agent("balanze-cli/0.1.0")
        // Bound a single stalled request - fail_fast() stops retries, not a hung
        // connection (AGENTS.md §3.1).
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    fetch_oauth_read_only_with(&client, ANTHROPIC_API_BASE, oauth, || async {
        tokio::task::spawn_blocking(|| {
            let source = locate_credentials()?;
            load_from_source(&source).map(|credentials| credentials.claude_ai_oauth)
        })
        .await?
        .map_err(Into::into)
    })
    .await
    .map(Some)
}

/// Fetch once with Claude Code's current bearer. A 401 may race Claude Code
/// rotating that bearer, so re-read its read-only credential once and retry
/// only when the access token changed. Balanze never refreshes or writes it.
async fn fetch_oauth_read_only_with<F, Fut>(
    client: &reqwest::Client,
    api_base: &str,
    oauth: CredentialsClaudeAiOauth,
    reload: F,
) -> Result<ClaudeOAuthSnapshot>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<CredentialsClaudeAiOauth>>,
{
    if oauth.is_expired_at(Utc::now()) {
        return Err(OAuthError::CredentialExpiredReadOnly.into());
    }

    let policy = backoff::BackoffPolicy::fail_fast();
    let first_token = oauth.access_token.clone();
    match fetch_usage(
        client,
        api_base,
        &oauth.access_token,
        oauth.subscription_type,
        oauth.rate_limit_tier,
        &policy,
    )
    .await
    {
        Ok(snapshot) => {
            info!("oauth: fetched {} cadence bars", snapshot.cadences.len());
            Ok(snapshot)
        }
        Err(OAuthError::AuthExpired) => {
            let current = reload().await?;
            if current.is_expired_at(Utc::now()) || current.access_token == first_token {
                return Err(OAuthError::CredentialExpiredReadOnly.into());
            }
            match fetch_usage(
                client,
                api_base,
                &current.access_token,
                current.subscription_type,
                current.rate_limit_tier,
                &policy,
            )
            .await
            {
                Ok(snapshot) => {
                    info!(
                        "oauth: fetched {} cadence bars after credential re-read",
                        snapshot.cadences.len()
                    );
                    Ok(snapshot)
                }
                Err(OAuthError::AuthExpired) => Err(OAuthError::CredentialExpiredReadOnly.into()),
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// Resolve the OpenAI admin key via [`keychain::resolve_openai_key`] (env
/// override, else keychain). `Ok(None)` = not configured; `Err` = a real
/// keychain failure. Thin `anyhow` adapter over the shared resolver, kept as
/// the crate-local name used by the snapshot fetch and statusline self-compose.
pub(crate) fn resolve_openai_key() -> Result<Option<String>> {
    Ok(keychain::resolve_openai_key()?)
}

/// Production OpenAI base, overridable via `BALANZE_OPENAI_API_BASE` (a test
/// seam; lets integration tests point the self-compose fetch at wiremock).
fn openai_api_base() -> String {
    openai_client::api_base_url()
}

/// Fetch this-month OpenAI costs if the user has configured an admin key.
///
/// Source order:
///   1. `BALANZE_OPENAI_KEY` env var (documented override; takes precedence
///      over the keychain - see AGENTS.md §3.4)
///   2. OS keychain entry `openai_api_key`
///   3. None -> "not configured"
///
/// Returns `Ok(None)` when nothing is configured; `Err` only for real
/// fetch failures (401, 403, network, etc.).
async fn live_fetch_openai() -> Result<Option<OpenAiCosts>> {
    let key = match resolve_openai_key()? {
        Some(k) => k,
        None => return Ok(None),
    };
    match gated_costs_this_month(&openai_api_base(), &key, std::time::Duration::from_secs(30)).await
    {
        Ok(costs) => {
            info!(
                "openai: fetched costs total_micro_usd={} buckets={} truncated={}",
                costs.total_micro_usd,
                costs.by_line_item.len(),
                costs.truncated
            );
            Ok(Some(costs))
        }
        // Shared admin-key hint, kept in lockstep with the watcher poller.
        Err(e) => match e.admin_key_hint() {
            Some(hint) => Err(anyhow!("{hint}")),
            None => Err(e.into()),
        },
    }
}

/// The real cross-provider sources for the statusline self-compose path.
/// Codex = local files; OpenAI = Admin Costs API behind a short timeout. Calls
/// NEITHER the Anthropic OAuth path NOR `snapshot_composer::compose` (AGENTS.md §3.1).
pub(crate) struct LiveCrossSources {
    /// Resolved at most once per statusline invocation, and only when the OpenAI
    /// segment is wanted. The same owned value drives both the provider-owned
    /// request identity and Authorization header. `Ok(None)` when the segment is
    /// off - the key is never read in that case.
    openai_key: Result<Option<String>, String>,
    openai_api_base: String,
    #[cfg(test)]
    openai_cache_dir: Option<std::path::PathBuf>,
}

impl LiveCrossSources {
    /// Build the sources for one statusline turn. `want_openai` gates the
    /// keychain read: when the OpenAI segment is off, the key is left `Ok(None)`
    /// unread, since Codex composition never uses it and reading it would prompt
    /// or add latency on macOS every turn (AGENTS.md §3.1: the politest call is
    /// the one not made).
    pub(crate) fn resolve(want_openai: bool) -> Self {
        Self {
            openai_key: if want_openai {
                resolve_openai_key().map_err(|error| error.to_string())
            } else {
                Ok(None)
            },
            openai_api_base: openai_api_base(),
            #[cfg(test)]
            openai_cache_dir: None,
        }
    }

    #[cfg(test)]
    fn from_resolved(
        openai_key: Result<Option<String>, String>,
        openai_api_base: String,
        openai_cache_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            openai_key,
            openai_api_base,
            openai_cache_dir: Some(openai_cache_dir),
        }
    }
}

impl statusline_render::CrossSources for LiveCrossSources {
    async fn openai_cell(&self) -> statusline_render::OpenAiCell {
        let key = match &self.openai_key {
            Ok(Some(key)) => key,
            Ok(None) => return statusline_render::OpenAiCell::default(),
            Err(error) => {
                tracing::debug!("statusline: OpenAI key resolution failed: {error}");
                return statusline_render::OpenAiCell::default();
            }
        };
        #[cfg(test)]
        let result = if let Some(cache_dir) = &self.openai_cache_dir {
            openai_client::gated_costs_this_month_with_cache(
                &self.openai_api_base,
                key,
                std::time::Duration::from_secs(3),
                cache_dir.clone(),
            )
            .await
        } else {
            gated_costs_this_month(
                &self.openai_api_base,
                key,
                std::time::Duration::from_secs(3),
            )
            .await
        };
        #[cfg(not(test))]
        let result = gated_costs_this_month(
            &self.openai_api_base,
            key,
            std::time::Duration::from_secs(3),
        )
        .await;

        match result {
            Ok(costs) => statusline_render::OpenAiCell {
                total_micro_usd: Some(costs.total_micro_usd),
                partial: costs.truncated,
                stale: false,
            },
            Err(error) => {
                let total_micro_usd = error.cached_total_micro_usd();
                let partial = error
                    .cached_full_costs()
                    .is_some_and(|costs| costs.truncated);
                tracing::debug!("statusline: OpenAI self-compose deferred: {error}");
                statusline_render::OpenAiCell {
                    total_micro_usd,
                    partial,
                    stale: total_micro_usd.is_some(),
                }
            }
        }
    }

    async fn codex_windows(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> statusline_render::CodexWindows {
        const CODEX_READ_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);
        let deadline = std::time::Instant::now() + CODEX_READ_BUDGET;
        let (send, receive) = tokio::sync::oneshot::channel();
        if let Err(error) = std::thread::Builder::new()
            .name("balanze-codex-statusline".to_string())
            .spawn(move || {
                let _ = send.send(codex_local::read_codex_quota_until(deadline));
            })
        {
            tracing::debug!("statusline: failed to start Codex read worker: {error}");
            return statusline_render::CodexWindows::default();
        }
        let read = tokio::time::timeout(CODEX_READ_BUDGET, receive).await;
        let quota = match read {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                tracing::debug!("statusline: Codex read worker closed: {error}");
                return statusline_render::CodexWindows::default();
            }
            Err(_) => {
                tracing::debug!(
                    ?CODEX_READ_BUDGET,
                    "statusline: Codex read exceeded foreground budget"
                );
                return statusline_render::CodexWindows::default();
            }
        };
        match quota {
            Ok(Some(q)) => statusline_render::CodexWindows {
                five_hour: q.five_hour().map(|w| w.used_percent as f32),
                weekly: q.weekly_or_other().map(|w| w.used_percent as f32),
                // The read is fresh; the rollout behind it may not be. Without
                // this the statusline printed a week-old figure unmarked.
                stale: q.any_window_expired(now),
            },
            _ => statusline_render::CodexWindows::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use statusline_render::CrossSources as _;

    /// Serializes env-mutating tests in this module. `cargo nextest` isolates
    /// each test in its own process, but plain `cargo test` shares one, so the
    /// lock keeps both runners honest.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn one_shot_jsonl_keeps_complete_records_across_corruption_and_partial_tails() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let fixture =
            include_str!("../tests/fixtures/claude/projects/test-project/session-001.jsonl");
        let mut bytes = fixture.as_bytes().to_vec();
        bytes.extend_from_slice(b"\n{broken json}\n\xff\n{\"type\":\"assistant\",\"model\":\"");
        bytes.extend_from_slice(&[0xe2, 0x82]);
        std::fs::write(&path, bytes).unwrap();

        let mut expected = claude_parser::parse_str(fixture).unwrap();
        dedup_events(&mut expected);
        let roots = vec![dir.path().to_path_buf()];
        let (actual, files) = load_claude_events_from_roots(&roots).unwrap();
        assert_eq!(files, 1);
        assert_eq!(
            actual, expected,
            "one corrupt record must not discard valid usage"
        );

        let mut live = claude_parser::IncrementalParser::new();
        let mut live_events = live.read_incremental(&path).unwrap().events().to_vec();
        dedup_events(&mut live_events);
        assert_eq!(actual, live_events);

        // Finish the malformed tail, then append a valid, distinct usage event.
        let record = fixture
            .lines()
            .find(|line| line.contains("\"assistant\""))
            .unwrap();
        let record = record
            .replace("msg_", "new_msg_")
            .replace("req_", "new_req_");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "\n{record}").unwrap();
        live_events.extend(
            live.read_incremental(&path)
                .unwrap()
                .events()
                .iter()
                .cloned(),
        );
        dedup_events(&mut live_events);
        let (after, _) = load_claude_events_from_roots(&roots).unwrap();
        assert_eq!(after, live_events);
        assert!(
            after.len() > actual.len(),
            "valid appends must remain visible"
        );
    }

    /// The keychain read is gated on `want_openai`: with the OpenAI segment off,
    /// the key is left unread (`Ok(None)`) even when one is configured, so the
    /// default statusline never touches the OpenAI keychain on a self-compose
    /// turn. `resolve(true)` still resolves the configured key.
    /// Removes an env var on drop, so the cleanup runs even if an assertion
    /// between set and remove panics. nextest (the project gate) already
    /// isolates each test in its own process, so a panic can't poison a sibling
    /// there; this keeps a shared-process `cargo test` run honest too.
    struct EnvGuard(&'static str);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: the test holding this guard also holds ENV_LOCK, which
            // serializes env mutation across this module's tests.
            unsafe { std::env::remove_var(self.0) };
        }
    }

    #[test]
    fn resolve_skips_the_key_read_when_openai_is_not_wanted() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: ENV_LOCK serializes env-mutating tests in this module. The
        // EnvGuard restores it on drop, before ENV_LOCK is released (drop runs
        // in reverse declaration order), even if an assertion below panics.
        unsafe { std::env::set_var("BALANZE_OPENAI_KEY", "sk-should-not-be-read") };
        let _restore = EnvGuard("BALANZE_OPENAI_KEY");

        // Wanted -> the configured key is resolved (env takes precedence over the
        // keychain, so this is deterministic regardless of the dev machine).
        let on = LiveCrossSources::resolve(true);
        assert_eq!(on.openai_key, Ok(Some("sk-should-not-be-read".to_string())));

        // Not wanted -> the key is never read, so it stays Ok(None) despite one
        // being configured. This is the keychain read the demand gate elides.
        let off = LiveCrossSources::resolve(false);
        assert_eq!(off.openai_key, Ok(None));
    }

    fn oauth(token: &str) -> CredentialsClaudeAiOauth {
        CredentialsClaudeAiOauth {
            access_token: token.to_string(),
            refresh_token: None,
            expires_at: i64::MAX,
            subscription_type: Some("pro".to_string()),
            rate_limit_tier: None,
            scopes: Vec::new(),
        }
    }

    #[tokio::test]
    async fn statusline_uses_the_same_resolved_key_for_gate_and_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/organization/costs"))
            .and(header("authorization", "Bearer resolved-once"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"object":"page","data":[],"has_more":false,"next_page":null}"#,
                "application/json",
            ))
            .expect(1)
            .mount(&server)
            .await;
        let cache_dir = tempfile::tempdir().unwrap();
        let sources = LiveCrossSources::from_resolved(
            Ok(Some("resolved-once".to_string())),
            server.uri(),
            cache_dir.path().to_path_buf(),
        );
        assert_eq!(sources.openai_cell().await.total_micro_usd, Some(0));
    }

    #[tokio::test]
    async fn statusline_key_resolution_failure_skips_openai_gate_state() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/organization/costs"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let cache_dir = tempfile::tempdir().unwrap();
        let sources = LiveCrossSources::from_resolved(
            Err("keychain unavailable".to_string()),
            server.uri(),
            cache_dir.path().to_path_buf(),
        );

        assert_eq!(
            sources.openai_cell().await,
            statusline_render::OpenAiCell::default()
        );
        assert_eq!(std::fs::read_dir(cache_dir.path()).unwrap().count(), 0);
        server.verify().await;
    }

    #[tokio::test]
    async fn oauth_401_rereads_rotated_bearer_once() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/oauth/usage"))
            .and(header("authorization", "Bearer old-token"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/oauth/usage"))
            .and(header("authorization", "Bearer new-token"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"five_hour":{"utilization":23.0,"resets_at":"2026-05-13T18:00:00+00:00"}}"#,
                "application/json",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let snapshot =
            fetch_oauth_read_only_with(&client, &server.uri(), oauth("old-token"), || async {
                Ok(oauth("new-token"))
            })
            .await
            .unwrap();

        assert_eq!(snapshot.cadences.len(), 1);
        assert_eq!(snapshot.cadences[0].key, "five_hour");
    }

    #[tokio::test]
    async fn oauth_401_preserves_transient_reread_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/oauth/usage"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let error =
            fetch_oauth_read_only_with(&client, &server.uri(), oauth("old-token"), || async {
                Err(anyhow!("temporary credential read failure"))
            })
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("temporary credential read failure")
        );
    }

    #[tokio::test]
    async fn oauth_401_does_not_retry_an_unchanged_bearer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/oauth/usage"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let error =
            fetch_oauth_read_only_with(&client, &server.uri(), oauth("old-token"), || async {
                Ok(oauth("old-token"))
            })
            .await
            .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<OAuthError>(),
            Some(OAuthError::CredentialExpiredReadOnly)
        ));
    }
}
