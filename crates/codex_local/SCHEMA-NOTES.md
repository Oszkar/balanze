# Codex JSONL — Schema Notes

**Spike date**: 2026-05-14
**Spike type**: Phase A pre-implementation, mandated by /plan-eng-review outside voice
**Sample data**: 3 sessions from author's machine, ~120KB each, 12-13 lines each, generated during the office-hours codex test calls earlier the same day

This file documents what we learned. It supersedes the assumption (in the design doc and earlier eng review) that `codex_local` mirrors `claude_parser` 1:1.

---

## File layout

```
~/.codex/sessions/
  └── {YYYY}/
      └── {MM}/
          └── {DD}/
              └── rollout-{ISO8601-timestamp}-{session-uuid}.jsonl
```

Example:
```
~/.codex/sessions/2026/05/14/rollout-2026-05-14T15-23-10-019e2527-42f8-78a0-8ffa-fcee353f8173.jsonl
```

**Differences from Claude Code:**
- Claude: `~/.claude/projects/{project-slug}/{session-uuid}.jsonl`
- Codex: `~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-{timestamp}-{uuid}.jsonl`

Codex nests by date; Claude nests by project. **The walker pattern in codex_local must descend 3 levels deep** (year / month / day) before finding `*.jsonl` files. claude_parser's flat-subdirectory walker pattern does NOT apply directly.

---

## Line types observed

Each JSONL line has a top-level `type` field:

| type | What it is | Useful for v0.1? |
|---|---|---|
| `session_meta` | First line; session id, cwd, originator, cli_version, model_provider, base_instructions | Metadata only |
| `event_msg` (subtype `task_started`) | One per turn start; turn_id, started_at, model_context_window | Maybe (for window math) |
| `event_msg` (subtype `token_count`) | **Per-turn token counts + rate limits** | **YES — load-bearing** |
| `event_msg` (subtype `task_complete`) | One per turn end | Maybe |
| `response_item` (subtype `message`) | Actual conversation turns; role: developer/user/assistant/system | No — content, not billing |

The `token_count` event_msg is the only line with billing-relevant data.

---

## The token_count event (canonical example)

```json
{
  "timestamp": "2026-05-14T06:23:25.393Z",
  "type": "event_msg",
  "payload": {
    "type": "token_count",
    "info": {
      "total_token_usage": {
        "input_tokens": 29331,
        "cached_input_tokens": 1920,
        "output_tokens": 98,
        "reasoning_output_tokens": 40,
        "total_tokens": 29429
      },
      "last_token_usage": {
        "input_tokens": 29331,
        "cached_input_tokens": 1920,
        "output_tokens": 98,
        "reasoning_output_tokens": 40,
        "total_tokens": 29429
      },
      "model_context_window": 258400
    },
    "rate_limits": {
      "limit_id": "codex",
      "limit_name": null,
      "primary": {
        "used_percent": 3.0,
        "window_minutes": 10080,
        "resets_at": 1779344602
      },
      "secondary": null,
      "credits": null,
      "plan_type": "go",
      "rate_limit_reached_type": null
    }
  }
}
```

### Key fields for v0.1's "Codex quota %" cell

- `payload.rate_limits.primary.used_percent` — **the value the user sees** (e.g. 3.0 means 3%).
- `payload.rate_limits.primary.window_minutes` — 10080 = 7 days. This is Codex CLI's only rolling window.
- `payload.rate_limits.primary.resets_at` — unix timestamp; convert to `DateTime<Utc>` for "resets in 4d 7h" display.
- `payload.rate_limits.plan_type` — "go" (ChatGPT Go), probably "pro" (ChatGPT Pro), etc. Display only.
- `payload.rate_limits.secondary` — null in this sample. May contain a sub-window (5h?) for higher-tier plans. Treat as `Option<RateLimitWindow>`.

### Fields possibly useful for v0.2+

- `last_token_usage.input_tokens` etc. — per-turn token counts. Useful if/when we ever compute "if Codex were billed by API rates, this is what you'd spend" (not in v0.1 — Codex side is quota %, not $).
- `total_token_usage` — cumulative per session. Useful for per-session reports.

### Fields NOT in Codex JSONL

- ❌ No `message_id` field.
- ❌ No `requestId` field.
- ❌ No per-event cost in $.
- ❌ No `cache_creation_input_tokens` / `cache_read_input_tokens` separation (only `cached_input_tokens` as one field).

---

## Critical findings — what changes for `codex_local`

### 1. No dedup module needed.

Claude Code's `(message_id, request_id)` dedup exists because Anthropic streams each assistant message multiple times — `claude_parser::dedup_events` collapses ~50% redundancy. **Codex does not stream-duplicate.** Each event_msg has a unique `turn_id`; no fields match the (msg_id, req_id) shape because Codex doesn't even have those fields. **Drop the planned `dedup` module from codex_local v0.1 entirely.** Save the 8 dedup tests; they'd be asserting the identity function.

### 2. The data model is "quota snapshot", not "stream of events".

`claude_parser` produces `Vec<UsageEvent>` because every assistant message is a billable event. `codex_local` should produce a `CodexQuotaSnapshot` — a single value extracted from the most recent `token_count` event in the most recent session file. The 4-quadrant cell ("Codex %") needs ONE number, not a stream.

### 3. Walker is simpler but deeper.

claude_parser walks `~/.claude/projects/{project-slug}/*.jsonl` (1 level). codex_local must walk `~/.codex/sessions/{YYYY}/{MM}/{DD}/*.jsonl` (3 levels). The pattern is "find the most recent file by mtime" — order matters, but recursion is straightforward.

### 4. IncrementalParser is probably unnecessary.

In claude_parser, IncrementalParser exists to avoid re-parsing 100MB of JSONL on every watcher tick. For codex_local v0.1, we just need the LAST `token_count` event from the LATEST session file. Read the file once (it's ~120KB), tail-scan for the latest event, return. **Drop the IncrementalParser pattern from codex_local v0.1**. Add it back in v0.2 if profiling shows it matters.

### 5. CodexEvent → CodexQuotaSnapshot.

The Issue 1 decision (separate type, not UsageEvent) is even more justified than we knew. The right type is:

```rust
pub struct CodexQuotaSnapshot {
    pub observed_at: DateTime<Utc>,         // from event_msg.timestamp
    pub session_id: String,                 // from session_meta.payload.id
    pub used_percent: f64,                  // primary.used_percent
    pub window_duration_minutes: u64,       // primary.window_minutes
    pub resets_at: DateTime<Utc>,           // primary.resets_at (unix → DateTime)
    pub plan_type: String,                  // "go", "pro", etc.
    pub secondary: Option<RateLimitWindow>, // for future 5h-window plans
    pub rate_limit_reached: bool,           // from rate_limit_reached_type
}

pub struct RateLimitWindow {
    pub used_percent: f64,
    pub window_duration_minutes: u64,
    pub resets_at: DateTime<Utc>,
}
```

### 6. Public API shrinks dramatically.

Pre-spike plan: `find_codex_sessions_dir`, `find_codex_sessions`, `parse_str`, `dedup_events`, `IncrementalParser`. ~9 source files, 39 tests.

Post-spike plan:
- `find_codex_sessions_dir() -> Result<PathBuf, ParseError>` (via `directories::UserDirs`, honors `CODEX_CONFIG_DIR`).
- `find_latest_session(root: &Path) -> Result<Option<PathBuf>, ParseError>` (3-level walk, mtime desc, return first).
- `read_latest_quota_snapshot(path: &Path) -> Result<Option<CodexQuotaSnapshot>, ParseError>` (open file, tail-scan for last token_count event_msg, parse).
- Convenience: `read_codex_quota() -> Result<Option<CodexQuotaSnapshot>, ParseError>` (compose all three).

~3 source files (`walker.rs`, `parser.rs`, `types.rs` + `lib.rs`/`errors.rs`). **5-8 tests, not 10-39.**

---

## Test list (revised post-spike)

1. `find_codex_sessions_dir`: `CODEX_CONFIG_DIR` set → uses it.
2. `find_codex_sessions_dir`: no env var, `~/.codex/sessions/` exists → uses it (via `directories::UserDirs::home_dir().join(".codex/sessions")`).
3. `find_codex_sessions_dir`: directory missing → `Err(FileMissing)` (caller maps to `DegradedState::CodexDirMissing`).
4. `find_latest_session`: walks `YYYY/MM/DD/`, returns most-recent `rollout-*.jsonl` by mtime.
5. `find_latest_session`: empty `sessions/` dir → `Ok(None)`.
6. `read_latest_quota_snapshot`: parses canonical sample line correctly → `CodexQuotaSnapshot` with expected fields.
7. `read_latest_quota_snapshot`: file with zero `token_count` events (a session that crashed immediately) → `Ok(None)`.
8. `read_latest_quota_snapshot`: malformed JSON line → `Err(SchemaDrift { line, message })` — but parser continues scanning earlier lines for the latest valid token_count.

**Plus 1 smoke example** (gated behind `examples/`, not `tests/`): run against real `~/.codex/sessions/` on the author's machine, print the snapshot.

**Total: ~8 tests + 1 smoke.** Down from the post-outside-voice number of 10, and dramatically down from the pre-outside-voice number of 39.

---

## Open questions

1. **Multiple Codex CLI installs**: does Codex have an equivalent of `CLAUDE_CONFIG_DIR` that points its install at a non-default location? Spike didn't surface one. Honor `CODEX_CONFIG_DIR` env var anyway (cheap), but don't promise it works the same way.

2. **Codex CLI version compatibility**: this schema was captured from `cli_version: "0.130.0"`. If Codex CLI bumps schema in a breaking way, our parser breaks. Treat schema drift as `DegradedState` per AGENTS.md §3.3.

3. **What about ChatGPT Pro / Pro+ plans?** Sample is from `plan_type: "go"`. Pro likely populates `secondary` with a 5h window. Capture as `Option<RateLimitWindow>` and display both windows in the CLI/UI if present.

4. **Token counts as a v0.2 enhancement**: the `last_token_usage` data per turn could feed a future "Codex API spend (estimated)" cell if we ever vendor OpenAI/Codex pricing. Out of scope for v0.1.

---

## Design doc + test plan implications

These changes need to land back in the design doc and the test plan:

- Design doc Premise 2 update: `codex_local` is a 3-file crate, not a 9-file mirror of claude_parser. ~200 LoC, not ~700.
- Test plan: codex_local test list shrinks from 10 to ~8.
- Test plan: drop the `IncrementalParser`-related tests from codex_local entirely.
- Test plan: drop the `dedup_events`-related tests from codex_local entirely.
- v0.1 success criteria: target shifts from "codex_local has ≥30 tests" to "codex_local has ≥8 tests + 1 smoke example, all passing against real ~/.codex/sessions/ data".

Apply these via the same /plan-eng-review flow if you want them re-reviewed; or apply directly if confident.
