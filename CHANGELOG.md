# Changelog

All notable changes to Balanze are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The
project is pre-1.0 — minor version bumps may include breaking changes; patch
bumps are bug fixes only.

## [Unreleased]

The v0.1 backend data layer; no tagged release yet. Once tagged, the contents
below move under a `[0.1.0] - YYYY-MM-DD` heading.

### Added
- `balanze` CLI: `status` / `set-openai-key` / `clear-openai-key` / `settings` / `help` subcommands.
- `claude_parser` crate — JSONL parser, dedup by `(message_id, request_id)`, `IncrementalParser` (byte-cursor reads with truncation + same-size-rewrite detection), `find_claude_projects_dir()` with XDG + `~/.claude/` + `~/.config/claude/` search.
- `anthropic_oauth` crate — calls `GET api.anthropic.com/api/oauth/usage` with the bearer token from `~/.claude/.credentials.json`. Maps known cadence keys to display labels; titlecases unknown keys for forward-compatibility.
- `openai_client` crate — calls `GET /v1/organization/costs` with an `sk-admin-…` bearer. Aggregates this-month spend by line item. Defensive redaction on error bodies before they reach `Display`.
- `window` crate — pure rolling-window math: 5-hour main window + 30-minute burn rate + per-model breakdown sorted by tokens.
- `state_coordinator` crate — actor scaffold (Snapshot + Sink trait + bounded-mpsc StateMsg loop). Tauri-free; the production `TauriSink` lands when `src-tauri` integrates.
- `settings` crate — non-secret `settings.json` with atomic writes (tmp + rename) and schema versioning.
- `keychain` crate — `keyring` wrapper for OS-keychain credential storage (only crate that imports `keyring`).
- README, AGENTS.md (operational contract), SECURITY.md, MIT LICENSE.
- CI on Windows + macOS (rustfmt + clippy + cargo test + svelte-check); Dependabot for cargo / npm / github-actions.

### Known issues
- **Windows keychain backend silently no-ops** (`keyring 3.6.3`). Workaround: `BALANZE_OPENAI_KEY` env var takes precedence over keychain reads. Real fix lands with the `keyring-core` (v4) migration in v0.2.
- **Anthropic OAuth bearer expires every ~7–8 hours.** Currently surfaced as `AuthExpired`; re-run `claude login` and retry. Refresh-token flow is v0.1.1 work.
- **`extra_usage` block from OAuth has unclear semantics** — the `used_credits` field doesn't reconcile with claude.ai's "$ spent this month" UI. Suppressed in pretty CLI output; still in `--json` for diagnostics. A claude.ai HAR investigation in v0.2 should resolve the units.

## Roadmap

- **v0.1.1** — OAuth refresh-token flow; anchor the cap window to OAuth's `resets_at` (instead of `now - 5h`); small CLI polish.
- **v0.2** — Tauri tray + popover UI; `predictor` crate (warm-up → uncertain → confident state machine on top of `window::WindowSummary`); `watcher` crate (notify + `IncrementalParser`); `keyring-core` migration to fix the Windows keychain bug; Anthropic Console source.
- **v0.3+** — Cross-device sync, Android, hosted wallboard. Out of scope for this milestone series.

<!--
  Until v0.1.0 is tagged, [Unreleased] points at the full commit history on
  main. After the first tag, change to:
    [Unreleased]: https://github.com/Oszkar/balanze/compare/v0.1.0...HEAD
-->
[Unreleased]: https://github.com/Oszkar/balanze/commits/main
