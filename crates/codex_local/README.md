# codex_local

Reads the user's local OpenAI Codex CLI session files
(`~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-*.jsonl`) and extracts the
latest rate-limit quota snapshot.

Fills the "Codex %" cell of Balanze's 4-quadrant matrix. Codex reports
plan-dependent rolling windows (5-hour and/or weekly); the crate classifies them by
duration and exposes `five_hour()` / `weekly()` / `worst_window()`. The
tray and compact CLI read the worst window; other surfaces consume the available windows.
ChatGPT Pro 100 (`prolite`) supplies only a weekly account-wide window. Missing windows are never synthesized.

## Parsing rules

Classify quota windows by duration, never by the `primary` or `secondary` slot. Either window may be absent. Only account-wide `limit_id: "codex"` (or absent/null for older records) supplies account quota; skip per-model limits. A non-string limit ID is schema drift. The parser and its fixtures define the supported wire format.

## Public API

```rust
use codex_local::{read_codex_quota, ParseError};

match read_codex_quota() {
    Ok(Some(snap)) => {
        // Both windows via snap.windows() / five_hour() / weekly();
        // worst_window() is the "how close to a limit" figure the tray uses.
        let w = snap.worst_window().expect("primary is always present");
        println!(
            "Codex: {:.1}% used ({:?} window), resets at {}",
            w.used_percent, w.kind(), w.resets_at,
        );
    }
    Ok(None) => {
        // Codex installed, scanned cleanly, just no quota data yet -
        // session probably crashed before any `token_count` event fired.
        println!("No Codex quota data in the latest session.");
    }
    Err(ParseError::FileMissing(_)) => {
        // Codex CLI not installed (sessions dir absent).
        println!("Codex CLI not installed.");
    }
    Err(ParseError::SchemaDrift { path, line, message }) => {
        // Codex CLI shipped a breaking schema change. Surface to the
        // user as "Codex data temporarily unavailable"; log the path/
        // line for the maintainer to debug.
        eprintln!("Codex schema drift at {}:{} - {}", path.display(), line, message);
    }
    Err(ParseError::IoError { path, source }) => {
        // Filesystem broke (permission denied, disk error). Loud.
        eprintln!("Codex read failed at {}: {}", path.display(), source);
    }
}
```

- [`read_codex_quota`] is the one-stop entry point. Walks the default
  sessions directory (or `CODEX_CONFIG_DIR` if set), finds the latest
  `rollout-*.jsonl` by mtime, parses the most recent `token_count`
  event_msg, returns `Option<CodexQuotaSnapshot>`.
- [`find_codex_sessions_dir`] / [`find_latest_session`] /
  [`read_latest_quota_snapshot`] are exposed separately for plumbing
  flexibility (testing, custom paths).
- All fallible functions return `Result<_, ParseError>` with three
  error variants (`FileMissing`, `IoError`, `SchemaDrift`) plus the
  `Ok(None)` "no data yet" case. The outcomes map onto the coordinator's
  degraded model - the `Snapshot::codex_quota_error` slot plus the
  `degraded_state` event (there is no `DegradedState` enum) - via
  `watcher::tasks::safety::codex_update`. See the crate-level `lib.rs`
  docs for the full failure-mode mapping.

## What's NOT in this crate (per spike findings)

- **No dedup module.** Codex doesn't stream-duplicate events the way
  Anthropic does; there's no `(message_id, request_id)` shape to dedup
  on, and one `token_count` event per turn is the actual emit pattern.
- **No `IncrementalParser`.** Each read scans candidate session files for the latest quota. Revisit incremental reads only if profiling identifies this path as a bottleneck.
- **No per-token cost computation.** Codex side is quota %, not $. The
  4-quadrant matrix's "OpenAI API $" cell is filled by `openai_client`
  (Admin Costs API). Per-turn token/context (`CodexTokenUsage`) stays internal
  through `#[serde(skip)]`. Credits (`CodexCredits`) are serialized and shown in
  the Grid and Cards popover layouts and CLI `status --sections`.
  The provider's decimal balance string is truncated to whole credits; zero or
  absent balances are hidden, while unlimited plans show `unlimited credits`.
  `status --json` exposes the nullable credits object and preserves reported zero balances.
  Credits reflect the last Codex session, not a periodic balance fetch. They are
  not a dollar estimate; this crate still computes no dollar cost.

## `CODEX_CONFIG_DIR`

Set this env var to override the default `~/.codex/` resolution. The
crate appends `sessions/` to the value (matches Codex CLI's own
`$CODEX_HOME` semantic).

```
CODEX_CONFIG_DIR=/path/to/codex-data balanze
```

## Tests

Tests cover walking, parsing, window classification, and source selection. Fixtures embed anonymized session records; run `cargo test -p codex_local`.

## License

MIT (workspace default). No vendored data; the crate reads files on
the user's machine and produces typed snapshots from them.
