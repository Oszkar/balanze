# Changelog

All notable changes to Balanze are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The
project is pre-1.0 — minor version bumps may include breaking changes; patch
bumps are bug fixes only.

## [Unreleased]

v0.1 — **"Data"**: a complete, honest four-quadrant data layer as a CLI. No
tagged release yet (the maintainer is dogfooding before tagging). Once tagged,
the contents below move under a `[0.1.0] - YYYY-MM-DD` heading.

### Added
- **`balanze-cli`** binary. Subcommands: `status` (default — 4-quadrant
  compact view with a confidence legend) / `setup` / `set-openai-key` /
  `clear-openai-key` / `settings` / `help`. `status` takes `--sections`
  (per-source detail), `--json` (machine Snapshot; wins over `--sections`),
  and `-v` (account-identifying fields); `--sections` / `--json` are also
  accepted as bare top-level shortcuts (e.g. `balanze-cli --json`).
- **Four-quadrant matrix**: Anthropic quota % (OAuth) · Anthropic API $
  *estimated* (JSONL × LiteLLM prices — subscription leverage, not real spend)
  · OpenAI Codex quota % (`~/.codex/sessions/`) · OpenAI API $ real billed
  spend (Admin Costs API).
- `claude_parser` crate — JSONL parser, dedup by `(message_id, request_id)`, `IncrementalParser` (byte-cursor reads with truncation + same-size-rewrite detection), `find_claude_projects_dir()` with XDG + `~/.claude/` + `~/.config/claude/` search.
- `claude_cost` crate — pure JSONL→estimated-$ synthesis against a vendored LiteLLM Anthropic price subset (MIT; `build.rs` stamps table provenance). Infallible: unknown models route to `skipped_models`. Output is explicitly labelled an estimate / subscription leverage, never presented as real spend.
- `anthropic_oauth` crate — calls `GET api.anthropic.com/api/oauth/usage` with the bearer from `~/.claude/.credentials.json`. Maps known cadence keys to display labels; titlecases unknown keys. Credentials carry a redacting `Debug`; error bodies are redacted before `Display`.
- `openai_client` crate — `GET /v1/organization/costs` with an `sk-admin-…` bearer. Aggregates this-month spend by line item. Defensive `sk-`-pattern redaction on error bodies.
- `codex_local` crate — reads `~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-*.jsonl`, extracts the latest `rate_limits.primary`. Single-snapshot (no streaming/dedup in v0.1). Honors `CODEX_CONFIG_DIR`.
- `window` crate — pure rolling-window math: 5-hour main window + 30-minute burn rate + per-model breakdown.
- `state_coordinator` crate — actor scaffold (Snapshot + Sink trait + bounded-mpsc StateMsg loop). Tauri-free; the production `TauriSink` lands with the v0.3 UI.
- `settings` crate — non-secret `settings.json` with atomic writes (tmp + rename) and schema versioning.
- `keychain` crate — `keyring` wrapper for OS-keychain credential storage (only crate that imports `keyring`).
- `balanze-cli setup` — interactive auth wizard: checks Anthropic OAuth + Codex presence, prompts for the OpenAI admin key (masked input via `rpassword`), validates it live, stores it.
- A 4-test end-to-end integration suite (`integration_4quadrant.rs`) exercising the real composition path against committed fixtures.
- README, AGENTS.md (operational contract), SECURITY.md, MIT LICENSE.
- CI on Windows + macOS (rustfmt + clippy + cargo test + svelte-check); Dependabot for cargo / npm / github-actions.

### Changed
- The CLI binary is **`balanze-cli`**, not `balanze`. `balanze` is reserved for the future src-tauri tray app to avoid a workspace build-artifact collision.
- Pre-tag cleanup (multi-agent review follow-up): de-flaked the integration test (deterministic `now`), sharpened the compact estimate label + legend, redacted a serde-error log path and the OAuth `Debug`/error-body surfaces, and closed several §6 validation-matrix test gaps.

### Known issues
- **Windows keychain backend silently no-ops** (`keyring 3.6.3`). Workaround: `BALANZE_OPENAI_KEY` env var takes precedence over keychain reads. Real fix lands with the `keyring` → `keyring-core` (v4) migration in **v0.3** (it rides with the settings UI that exercises the key-input box on both platforms).
- **Anthropic OAuth bearer expires every ~7–8 hours.** Currently surfaced as `AuthExpired`; re-run `claude login` and retry. Refresh-token flow is v0.1.1 work.
- **`extra_usage` block from OAuth has unclear semantics** — the `used_credits` field doesn't reconcile with claude.ai's "$ spent this month" UI. Suppressed in pretty CLI output; still in `--json` for diagnostics. The v0.3 Anthropic Console (HAR) investigation should resolve the units.
- **Anthropic API $ is an estimate, not real spend.** The official Usage & Cost API is enterprise/org-admin-gated (Phase-0 spike: NO-GO for the modal user). The JSONL-derived figure is the honest best-available signal and is labelled as such; a real-spend source is a v0.2+ research note contingent on enterprise access.

## Roadmap

Theme per phase: **Data → Liveness → UI → Distribution**.

- **v0.1 — Data** (this milestone): the four-quadrant CLI above.
- **v0.1.1** — OAuth refresh-token flow; anchor the cap window to OAuth's `resets_at` (instead of `now - 5h`); small CLI polish.
- **v0.2 — Liveness** — `watcher` crate (notify + debounce + `IncrementalParser` + safety poll); `predictor` crate (EWMA + warm-up state machine on `window::WindowSummary`); `--watch`; `statusline`.
- **v0.3 — UI** — Tauri tray + popover; settings UI; `keyring` → `keyring-core` v4 migration (fixes the Windows keychain bug); degraded-state events; dashboard window; alerts; Anthropic Console cookie-paste source.
- **v0.4 — Distribution** — signed binaries (Windows cert, macOS notarization), Homebrew tap, WinGet manifest, Tauri auto-update.
- **v1+** — Ubuntu GNOME, cross-device sync, Android companion, hosted wallboard.

<!--
  Until v0.1.0 is tagged, [Unreleased] points at the full commit history on
  main. After the first tag, change to:
    [Unreleased]: https://github.com/Oszkar/balanze/compare/v0.1.0...HEAD
-->
[Unreleased]: https://github.com/Oszkar/balanze/commits/main
