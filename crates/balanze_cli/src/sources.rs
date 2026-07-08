//! The production `SnapshotSources`: real network + filesystem + keychain.
//! `build_snapshot` runs `snapshot_composer::compose` over `LiveSources`; the
//! per-source helpers are the I/O adapters (AGENTS.md §4 #8 - glue, not logic).

use anyhow::{Result, anyhow};
use chrono::{Duration, Utc};
use std::fs;

use anthropic_oauth::{
    ClaudeOAuthSnapshot, DEFAULT_API_BASE as ANTHROPIC_API_BASE, OAuthError, REFRESH_MARGIN,
    fetch_usage, load_from_source, locate_credentials, refresh_and_persist, token_needs_refresh,
};
use claude_parser::{
    UsageEvent, dedup_events, find_all_claude_projects_dirs, find_claude_projects_dir,
    find_jsonl_files, parse_str,
};
use openai_client::{
    DEFAULT_API_BASE as OPENAI_API_BASE, OpenAiCosts, OpenAiError, costs_this_month,
};
use state_coordinator::Snapshot;
use tracing::{info, warn};

// The source-orchestration policy now lives in `snapshot_composer::compose`
// (AGENTS.md §4 #8): the CLI runs it via `LiveSources`, the future watcher
// will run it via its own `SnapshotSources` impl, and `integration_4quadrant`
// runs it via `FixtureSources` - one policy, no silent divergence.
pub(crate) async fn build_snapshot() -> Snapshot {
    snapshot_composer::compose(&LiveSources, Utc::now()).await
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
    async fn fetch_oauth(&self) -> Result<ClaudeOAuthSnapshot> {
        live_fetch_oauth().await
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
/// Returns `(events, files_scanned)`. Files that fail to read or parse
/// are logged (warn level) but don't fail the whole call - matches the
/// existing tolerant policy.
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

    let mut files = Vec::new();
    let mut walk_err = None;
    for root in &roots {
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
    if files.is_empty() {
        if let Some(e) = walk_err {
            return Err(e.into());
        }
    }
    info!(
        "jsonl: scanning {} files across {} root(s)",
        files.len(),
        roots.len()
    );

    let mut all_events: Vec<UsageEvent> = Vec::new();
    for path in &files {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                warn!("jsonl: skipping {} ({e})", path.display());
                continue;
            }
        };
        match parse_str(&content) {
            Ok(events) => all_events.extend(events),
            Err(e) => warn!("jsonl: parse error in {} ({e})", path.display()),
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

async fn live_fetch_oauth() -> Result<ClaudeOAuthSnapshot> {
    // locate+load is sync I/O (a file read, or a `security` subprocess on
    // macOS that can block on a Keychain access prompt), so run it on a
    // blocking worker rather than stalling a tokio runtime thread (AGENTS.md
    // §2.1).
    let (source, creds) = tokio::task::spawn_blocking(|| {
        let source = locate_credentials()?;
        let creds = load_from_source(&source)?;
        Ok::<_, OAuthError>((source, creds))
    })
    .await??;
    let mut oauth = creds.claude_ai_oauth;
    let client = reqwest::Client::builder()
        .user_agent("balanze-cli/0.1.0")
        // Bound a single stalled request - fail_fast() stops retries, not a hung
        // connection (AGENTS.md §3.1).
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    // One-shot CLI must not block on provider backoff; the watcher passes standard().
    let policy = backoff::BackoffPolicy::fail_fast();

    // Refresh only a source we own (a file). The macOS Keychain entry is Claude
    // Code's - read-only (AGENTS.md §3.4): use the token while valid; if it has
    // already expired, surface an actionable error rather than refresh it.
    if let Some(path) = source.writable_path() {
        if token_needs_refresh(oauth.expires_at, Utc::now(), REFRESH_MARGIN) {
            info!("oauth: token expired/near-expiry - refreshing pre-flight");
            oauth = refresh_and_persist(&client, path, oauth, &policy).await?;
        }
    } else if token_needs_refresh(oauth.expires_at, Utc::now(), Duration::zero()) {
        return Err(OAuthError::CredentialExpiredReadOnly.into());
    }

    match fetch_usage(
        &client,
        ANTHROPIC_API_BASE,
        &oauth.access_token,
        oauth.subscription_type.clone(),
        oauth.rate_limit_tier.clone(),
        &policy,
    )
    .await
    {
        Ok(s) => {
            info!("oauth: fetched {} cadence bars", s.cadences.len());
            Ok(s)
        }
        Err(OAuthError::AuthExpired) => {
            // 401 despite the pre-flight check. For a file source, refresh +
            // retry once (bounded - the retry uses `?`, so a second AuthExpired
            // propagates, no loop). For the read-only Keychain source we can't
            // refresh, so surface the actionable error.
            let Some(path) = source.writable_path() else {
                return Err(OAuthError::CredentialExpiredReadOnly.into());
            };
            warn!("oauth: 401 despite pre-flight - one refresh+retry");
            let oauth = refresh_and_persist(&client, path, oauth, &policy).await?;
            let s = fetch_usage(
                &client,
                ANTHROPIC_API_BASE,
                &oauth.access_token,
                oauth.subscription_type,
                oauth.rate_limit_tier,
                &policy,
            )
            .await?;
            info!(
                "oauth: fetched {} cadence bars after refresh",
                s.cadences.len()
            );
            Ok(s)
        }
        Err(e) => Err(e.into()),
    }
}

/// Resolve the OpenAI admin key via [`keychain::resolve_openai_key`] (env
/// override, else keychain). `Ok(None)` = not configured; `Err` = a real
/// keychain failure. Thin `anyhow` adapter over the shared resolver, kept as
/// the crate-local name used by the snapshot fetch and the statusline
/// self-compose fingerprint.
pub(crate) fn resolve_openai_key() -> Result<Option<String>> {
    Ok(keychain::resolve_openai_key()?)
}

/// Production OpenAI base, overridable via `BALANZE_OPENAI_API_BASE` (a test
/// seam; lets integration tests point the self-compose fetch at wiremock).
fn openai_api_base() -> String {
    std::env::var("BALANZE_OPENAI_API_BASE").unwrap_or_else(|_| OPENAI_API_BASE.to_string())
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
    let client = reqwest::Client::builder()
        .user_agent("balanze-cli/0.1.0")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    // One-shot CLI must not block on provider backoff; watcher passes standard().
    match costs_this_month(
        &client,
        OPENAI_API_BASE,
        &key,
        &backoff::BackoffPolicy::fail_fast(),
    )
    .await
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
        Err(OpenAiError::AuthInvalid { .. }) => Err(anyhow!(
            "OpenAI admin key rejected (HTTP 401). Run `balanze-cli set-openai-key` with a fresh `sk-admin-...` key."
        )),
        Err(OpenAiError::InsufficientScope { .. }) => Err(anyhow!(
            "OpenAI returned 403. organization/costs requires an admin API key (`sk-admin-...`), not a project or service-account key. Generate one at https://platform.openai.com/settings/organization/admin-keys."
        )),
        Err(e) => Err(e.into()),
    }
}

/// The real cross-provider sources for the statusline self-compose path.
/// Codex = local files; OpenAI = Admin Costs API behind a short timeout. Calls
/// NEITHER the Anthropic OAuth path NOR `snapshot_composer::compose` (AGENTS.md §3.1).
pub(crate) struct LiveCrossSources;

impl statusline_render::CrossSources for LiveCrossSources {
    async fn fetch_openai_total_micro_usd(&self) -> Result<Option<i64>, String> {
        // Absent key -> no OpenAI cell (`Ok(None)`). A real resolver failure ->
        // `Err`, so self_compose serves the last-known value marked stale and
        // starts the cooldown instead of silently dropping the cell. Either way
        // the statusline never errors: self_compose handles both outcomes.
        let key = match resolve_openai_key() {
            Ok(Some(k)) => k,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        // Short timeout: the statusline runs every turn; never hang the prompt.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .map_err(|e| e.to_string())?;
        let costs = openai_client::costs_this_month(
            &client,
            &openai_api_base(),
            &key,
            &backoff::BackoffPolicy::fail_fast(),
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(Some(costs.total_micro_usd))
    }

    fn codex_windows(&self) -> (Option<f32>, Option<f32>) {
        match codex_local::read_codex_quota() {
            Ok(Some(q)) => (
                q.five_hour().map(|w| w.used_percent as f32),
                q.weekly().map(|w| w.used_percent as f32),
            ),
            _ => (None, None),
        }
    }
}
