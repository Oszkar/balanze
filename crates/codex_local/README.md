# codex_local

Reads the user's local OpenAI Codex CLI session files
(`~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-*.jsonl`) and extracts the
latest rate-limit quota snapshot.

Fills the "Codex %" cell of Balanze's 4-quadrant matrix
(`primary.used_percent` from the latest `token_count` event). The
schema details that drove the design live in `SCHEMA-NOTES.md` (this
directory) — re-read those if Codex CLI changes its JSONL format.

## Public API

```rust
use codex_local::{read_codex_quota, ParseError};

match read_codex_quota() {
    Ok(Some(snap)) => println!(
        "Codex: {:.1}% of {}d window, resets at {}",
        snap.primary.used_percent,
        snap.primary.window_duration_minutes / 1440,
        snap.primary.resets_at,
    ),
    Ok(None) => {
        // Codex installed, scanned cleanly, just no quota data yet —
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
        eprintln!("Codex schema drift at {}:{} — {}", path.display(), line, message);
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
  `Ok(None)` "no data yet" case. The five-way outcome maps onto the
  eventual `state_coordinator::DegradedState` enum (defined in v0.1
  step 5 when state_coordinator consumes codex_local's output). See
  the crate-level `lib.rs` docs for the full failure-mode mapping.

## What's NOT in this crate (per spike findings)

- **No dedup module.** Codex doesn't stream-duplicate events the way
  Anthropic does; there's no `(message_id, request_id)` shape to dedup
  on, and one `token_count` event per turn is the actual emit pattern.
  See `SCHEMA-NOTES.md` §"No dedup module needed".
- **No `IncrementalParser`.** v0.1 reads the latest session file
  cold each call (~120 KB, ~1 ms). If v0.2's watcher tier shows this
  is a hot loop, revisit; for v0.1 the simpler scan wins.
- **No per-token cost computation.** Codex side is quota %, not $. The
  4-quadrant matrix's "OpenAI API $" cell is filled by `openai_client`
  (Admin Costs API). If a "Codex API spend (estimated)" cell ever
  becomes a v0.2+ goal, the per-turn `last_token_usage` fields are
  available in the JSONL — that's a different crate's problem.

## `CODEX_CONFIG_DIR`

Set this env var to override the default `~/.codex/` resolution. The
crate appends `sessions/` to the value (matches Codex CLI's own
`$CODEX_HOME` semantic).

```
CODEX_CONFIG_DIR=/path/to/codex-data balanze
```

## Tests

12 unit tests across `walker.rs` (5) and `parser.rs` (7). Fixtures
embed anonymized real-data lines captured during the schema spike;
the original raw data lives at
`~/.gstack/projects/balanze/spike-codex-schema-20260514.md`.

## License

MIT (workspace default). No vendored data; the crate reads files on
the user's machine and produces typed snapshots from them.
