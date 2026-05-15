//! End-to-end integration test for the v0.1 4-quadrant pipeline.
//!
//! The eng-review test plan explicitly called for this (under "Critical
//! Paths" item 7): a test that exercises the full
//! claude_parser → claude_cost → Snapshot wiring and asserts that
//! Snapshot.anthropic_api_cost is non-zero against a committed fixture.
//! Without this, 175+ unit tests can pass while the wiring step that
//! actually composes them is silently broken.
//!
//! The fixtures live under `tests/fixtures/` and are committed: an
//! anonymized Claude Code JSONL with 3 assistant messages (known
//! token counts → predictable cost magnitudes) and a Codex rollout
//! file with a single `token_count` event_msg carrying a known
//! `used_percent: 17.5`.
//!
//! Test strategy: call the per-crate public APIs the same way
//! balanze_cli's `build_snapshot` calls them, populate a Snapshot,
//! assert the expected fields are populated. This is a finer-grained
//! test than running `balanze` as a subprocess but exercises the
//! actual composition contract.

use std::path::PathBuf;

use claude_cost::{compute_cost, load_bundled_prices};
use claude_parser::{dedup_events, find_jsonl_files, parse_str, UsageEvent};
use codex_local::{find_latest_session, read_latest_quota_snapshot};
use snapshot_composer::{compose, SnapshotSources};
use state_coordinator::{merge_partial, JsonlSnapshot, Snapshot, SourcePartial};
use window::{summarize_window, DEFAULT_BURN_WINDOW, DEFAULT_MIN_BURN_EVENTS, DEFAULT_WINDOW};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load_fixture_events() -> Vec<UsageEvent> {
    let claude_dir = fixture_root().join("claude/projects");
    let files = find_jsonl_files(&claude_dir).expect("fixture JSONL dir must exist");
    let mut events = Vec::new();
    for path in &files {
        let content = std::fs::read_to_string(path).expect("fixture readable");
        let parsed = parse_str(&content).expect("fixture JSONL parses cleanly");
        events.extend(parsed);
    }
    dedup_events(&mut events);
    events
}

#[test]
fn full_pipeline_populates_anthropic_api_cost_in_snapshot() {
    // Step 1: parse fixture JSONL (mirrors balanze_cli's
    // load_and_dedup_claude_events).
    let events = load_fixture_events();
    assert!(
        !events.is_empty(),
        "fixture should produce parseable events"
    );

    // Step 2: compute cost (mirrors balanze_cli's compute_anthropic_api_cost).
    let prices = load_bundled_prices().expect("bundled prices load");
    let cost = compute_cost(&events, &prices);

    // Step 3: populate a Snapshot via merge_partial — the same path the
    // coordinator will use (and that balanze_cli currently writes
    // directly).
    let now = chrono::Utc::now();
    let mut snapshot = Snapshot::empty(now);
    merge_partial(&mut snapshot, SourcePartial::AnthropicApiCost(cost.clone()));

    // Step 4: assert the contract the eng-review test plan specified.
    let saved = snapshot
        .anthropic_api_cost
        .as_ref()
        .expect("AnthropicApiCost should now be populated");
    assert!(
        saved.total_micro_usd > 0,
        "Snapshot.anthropic_api_cost.total_micro_usd must be > 0 with fixture data; \
         got {} (events: {}, per_model: {:?})",
        saved.total_micro_usd,
        events.len(),
        saved.per_model.iter().map(|m| &m.model).collect::<Vec<_>>(),
    );
    assert_eq!(
        snapshot.anthropic_api_cost_error, None,
        "successful merge_partial must clear the error slot"
    );

    // Spot-check structure: the fixture has 4 raw JSONL lines — 3 distinct
    // assistant messages (sonnet-4-6 ×2, haiku-4-5 ×1) plus 1 line that
    // duplicates msg_fixture_001's (message_id, request_id) with inflated
    // tokens. `load_fixture_events` runs `dedup_events`, so the pipeline
    // must see exactly 3: a `== 3` here now genuinely exercises dedup (a
    // regression that skipped it would yield 4 and the huge dup tokens
    // would also blow up total_micro_usd). Both surviving models are in
    // the bundled price table → 2 per_model rows, zero skipped.
    assert_eq!(
        saved.total_event_count, 3,
        "dedup must collapse the 4 raw lines (1 duplicate) to 3 events"
    );
    assert_eq!(saved.unparsed_event_count, 0, "no empty-model events");
    assert_eq!(saved.per_model.len(), 2, "2 distinct known models");
    assert!(
        saved.skipped_models.is_empty(),
        "fixture uses bundled-table models; got skipped: {:?}",
        saved.skipped_models
    );
    let models: Vec<&str> = saved.per_model.iter().map(|m| m.model.as_str()).collect();
    assert!(models.contains(&"claude-sonnet-4-6"), "got: {models:?}");
    assert!(models.contains(&"claude-haiku-4-5"), "got: {models:?}");
}

#[test]
fn full_pipeline_populates_claude_jsonl_in_snapshot() {
    // Mirrors balanze_cli's summarize_for_jsonl_snapshot — the window
    // math arm. Verifies the JSONL slot lights up too, so a future
    // refactor that breaks the shared-events plumbing fails this test.
    let events = load_fixture_events();
    // Fixed `now` one hour after the last fixture event (2026-05-15T10:02Z)
    // so all 3 fixtures fall inside the 5-hour main window deterministically.
    // Using Utc::now() here made the assertion tautological: once the fixture
    // timestamps aged out of the live window the count went to 0 and the old
    // `<= 3` bound passed vacuously, so a window-math regression would not
    // have been caught.
    let now = chrono::DateTime::parse_from_rfc3339("2026-05-15T11:02:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let window = summarize_window(
        &events,
        now,
        DEFAULT_WINDOW,
        DEFAULT_BURN_WINDOW,
        DEFAULT_MIN_BURN_EVENTS,
        None,
    );
    let jsonl = JsonlSnapshot {
        files_scanned: 1,
        window,
    };

    let mut snapshot = Snapshot::empty(now);
    merge_partial(&mut snapshot, SourcePartial::ClaudeJsonl(jsonl));

    let saved = snapshot
        .claude_jsonl
        .as_ref()
        .expect("ClaudeJsonl should populate");
    assert_eq!(saved.files_scanned, 1);
    // With the fixed `now` above, all 3 fixture events fall inside the
    // 5-hour main window, so the count is exact and deterministic. This
    // now actually exercises the window-math arm of the pipeline rather
    // than passing vacuously.
    assert_eq!(
        saved.window.total_events_in_window, 3,
        "all 3 fixture events fall in the 5h window relative to the fixed now"
    );
}

#[test]
fn full_pipeline_populates_codex_quota_in_snapshot() {
    let codex_dir = fixture_root().join("codex/sessions");
    let path = find_latest_session(&codex_dir)
        .expect("walker should find fixture session")
        .expect("fixture session present");
    let snap = read_latest_quota_snapshot(&path)
        .expect("fixture parses")
        .expect("token_count event present in fixture");

    let now = chrono::Utc::now();
    let mut snapshot = Snapshot::empty(now);
    merge_partial(&mut snapshot, SourcePartial::CodexQuota(snap));

    let saved = snapshot
        .codex_quota
        .as_ref()
        .expect("CodexQuota should populate");
    assert!(
        (saved.primary.used_percent - 17.5).abs() < 0.001,
        "fixture asserts used_percent: 17.5; got {}",
        saved.primary.used_percent
    );
    assert_eq!(saved.primary.window_duration_minutes, 10_080);
    assert_eq!(saved.plan_type, "go");
    assert!(!saved.rate_limit_reached);
}

#[test]
fn snapshot_serializes_with_new_cost_and_codex_fields() {
    // The `--json` output mode round-trips through serde. This test
    // specifically exercises the TWO fields this PR adds
    // (`anthropic_api_cost`, `codex_quota`) — the other two cells
    // (`claude_oauth`, `openai`) serialize via code that predates this
    // PR and is covered elsewhere. The point here is that adding the
    // new fields didn't break serialization (e.g., a missing Serialize
    // derive on a transitive type like claude_cost::Cost or
    // codex_local::CodexQuotaSnapshot).
    let events = load_fixture_events();
    let prices = load_bundled_prices().unwrap();
    let cost = compute_cost(&events, &prices);

    let codex_dir = fixture_root().join("codex/sessions");
    let codex_path = find_latest_session(&codex_dir).unwrap().unwrap();
    let codex_quota = read_latest_quota_snapshot(&codex_path).unwrap().unwrap();

    let now = chrono::Utc::now();
    let mut snapshot = Snapshot::empty(now);
    merge_partial(&mut snapshot, SourcePartial::AnthropicApiCost(cost));
    merge_partial(&mut snapshot, SourcePartial::CodexQuota(codex_quota));

    let json = serde_json::to_string_pretty(&snapshot).expect("snapshot serializes");

    // Spot-check the wire shape — these field names are the public
    // JSON contract for `balanze status --json`. Renaming any of them
    // is a breaking change for downstream consumers (none yet, but
    // pinning the contract now prevents accidental future drift).
    assert!(
        json.contains("\"anthropic_api_cost\""),
        "expected anthropic_api_cost field"
    );
    assert!(
        json.contains("\"codex_quota\""),
        "expected codex_quota field"
    );
    assert!(
        json.contains("\"total_micro_usd\""),
        "expected nested cost.total_micro_usd"
    );
    assert!(
        json.contains("\"used_percent\""),
        "expected nested codex_quota.primary.used_percent"
    );
    // serde_json::to_string_pretty emits "key": "value" with a space
    // between key and value; match that format precisely so a future
    // serializer swap (e.g. to a compact writer) catches the drift.
    assert!(
        json.contains("\"plan_type\": \"go\""),
        "expected plan_type from fixture; got:\n{json}"
    );
}

#[test]
fn window_anchors_to_supplied_reset() {
    use chrono::Duration as ChronoDuration;
    let events = load_fixture_events();
    let now = chrono::Utc::now();
    let reset = now + ChronoDuration::minutes(90);
    let anchored = summarize_window(
        &events,
        now,
        DEFAULT_WINDOW,
        DEFAULT_BURN_WINDOW,
        DEFAULT_MIN_BURN_EVENTS,
        Some(reset),
    );
    assert_eq!(anchored.window_start, reset - DEFAULT_WINDOW);
}

// ---------------------------------------------------------------------------
// Task 3: FixtureSources + compose() parity test
// ---------------------------------------------------------------------------

/// Zero-network `SnapshotSources` implementation that feeds `compose()` the
/// same committed fixtures the hand-rolled tests above use. This is the
/// anti-divergence guard (AGENTS.md §4 #8): the SAME `compose()` path that
/// `balanze_cli`'s `LiveSources` runs is exercised against fixtures, so a
/// change to `snapshot_composer::compose` that silently breaks the wiring
/// will fail here before it reaches production.
struct FixtureSources;

impl SnapshotSources for FixtureSources {
    async fn fetch_oauth(&self) -> anyhow::Result<anthropic_oauth::ClaudeOAuthSnapshot> {
        // No network in the integration test: oauth deliberately fails so
        // we also exercise compose()'s now-relative window fallback.
        anyhow::bail!("fixture: no oauth")
    }
    async fn load_claude_events(&self) -> anyhow::Result<(Vec<UsageEvent>, usize)> {
        Ok((load_fixture_events(), 1))
    }
    async fn fetch_codex_quota(&self) -> anyhow::Result<Option<codex_local::CodexQuotaSnapshot>> {
        let codex_dir = fixture_root().join("codex/sessions");
        let path = find_latest_session(&codex_dir)?.expect("fixture session present");
        Ok(read_latest_quota_snapshot(&path)?)
    }
    async fn fetch_openai(&self) -> anyhow::Result<Option<openai_client::OpenAiCosts>> {
        Ok(None)
    }
}

#[tokio::test]
async fn compose_parity_against_fixtures() {
    // Same fixed `now` as `full_pipeline_populates_claude_jsonl_in_snapshot`
    // so all 3 fixture events fall in the 5h window deterministically.
    let now = chrono::DateTime::parse_from_rfc3339("2026-05-15T11:02:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let snap = compose(&FixtureSources, now).await;

    // OAuth deliberately errored ⇒ error slot set, data None, window
    // falls back to now-relative.
    assert!(snap.claude_oauth.is_none());
    assert!(snap.claude_oauth_error.is_some());

    let jsonl = snap.claude_jsonl.as_ref().expect("jsonl populated");
    assert_eq!(jsonl.files_scanned, 1);
    assert_eq!(
        jsonl.window.total_events_in_window, 3,
        "all 3 fixture events fall in the 5h window for the fixed now"
    );

    let cost = snap
        .anthropic_api_cost
        .as_ref()
        .expect("anthropic_api_cost populated");
    assert!(cost.total_micro_usd > 0);
    assert_eq!(cost.total_event_count, 3, "dedup collapses 4 raw → 3");
    assert_eq!(snap.anthropic_api_cost_error, None);

    let codex = snap.codex_quota.as_ref().expect("codex populated");
    assert!((codex.primary.used_percent - 17.5).abs() < 0.001);

    assert!(snap.openai.is_none() && snap.openai_error.is_none());
}
