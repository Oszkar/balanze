# Changelog

All notable changes to Balanze are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The
project is pre-1.0 — minor version bumps may include breaking changes; patch
bumps are bug fixes only.

## [Unreleased]

### Added
- **Claude Code statusLine integration.** New `claude_statusline` crate +
  `balanze-cli statusline` subcommand: reads Claude Code's statusLine JSON and
  prints live 5h/7d subscription quota + session cost in your shell — zero-auth,
  no rate limit. `balanze-cli setup` offers to wire it (ask-first, never
  clobbers an existing statusLine, reversible).
- **Real pay-as-you-go overage surfaced.** If you enabled Anthropic
  "Extra usage", `balanze-cli` now shows your real billed overage
  (spent / limit / %) in both the compact grid and `--sections` — the
  exact figure claude.ai shows. Previously suppressed because its units
  were unverified; a reconciliation spike resolved it (cents; the
  claude.ai overage meter).

### Changed
- **The Anthropic API-$ estimate is now hard-labeled.** The JSONL ×
  list-price number is explicitly tagged "estimate — subscription
  leverage, NOT billed" and visually separated from the real overage, so
  a large estimate can't be misread as real spend.

### Fixed
- `anthropic_oauth` `ExtraUsage` docs no longer say the semantic is
  "unknown" (resolved: cents / overage meter); `claude_cost` no longer
  references a non-existent `Confidence::Estimated` type. Corrected the
  PRD's false "Claude Code records a per-event cost in the JSONL" premise.

## [0.1.1] - 2026-05-19

**v0.1.1 base** — Track A of the v0.2 roadmap (Liveness foundations). The
JSONL→estimate honesty redesign, statusline source, and the watcher/predictor
are later v0.2 tracks; see `docs/prd.md` Phase 2.

### Added
- **Proactive Anthropic OAuth refresh.** `anthropic_oauth` gained a
  refresh-token grant (`refresh_access_token`) and an atomic, anti-clobber
  credential write-back (`write_back`: tmp+rename, preserves permissions,
  reuses Anthropic's file, never regresses a concurrently-newer on-disk
  token). `balanze-cli` now refreshes the bearer pre-flight when it is
  expired or within a 5-minute margin, and recovers from a hard 401 with one
  refresh + retry — the bearer no longer hard-fails every ~7–8 h. Refresh
  failure still surfaces as `AuthExpired` (re-run `claude login`); no new
  `DegradedState`. Tokens are never logged; the refresh endpoint/client-id
  constants are gated by an `#[ignore]`'d real-endpoint smoke run pre-tag.
- `window::summarize_window` takes an optional `window_anchor`; the cap
  window is anchored to Anthropic's server-reported `five_hour` `resets_at`
  (half-open `[reset − 5h, reset)`), falling back to the legacy `now − 5h`
  when OAuth is unavailable — removing local clock-drift error from the cap
  math. `ClaudeOAuthSnapshot::five_hour_reset()` keeps the OAuth wire key in
  the schema-owning crate.

### Changed
- The secret surface expanded: `anthropic_oauth` is now a *writer* of
  `~/.claude/.credentials.json` (was read-only). The write obeys AGENTS.md
  §3.4 (atomic, perms-preserving, Anthropic's own file, OAuth fields only).

### Fixed
- `extra_usage` Known-issue note retargeted: the OAuth `extra_usage`
  reconciliation is now a scheduled v0.2 Track C spike (was a vague v0.3 HAR
  item) — see README / `docs/prd.md`.

**v0.2 Track B (de-risk)** — foundations for a poller, no user-facing
behavior change (the CLI is byte-identical; the new retry layer is inert
under the CLI's fail-fast policy). See `docs/prd.md` Phase 2.

### Added
- **`snapshot_composer` crate.** The source-orchestration policy
  (`build_snapshot`) is extracted behind a `SnapshotSources` trait into one
  `compose()` function. `balanze-cli` runs it via `LiveSources`; the future
  v0.2 watcher will run the *same* `compose()` via its own `SnapshotSources`
  — so the two composition paths cannot silently diverge (AGENTS.md §4 #8).
  A fixture-driven `compose_parity_against_fixtures` integration test guards
  it.
- **`backoff` crate.** Pure exponential-backoff policy
  (`standard` = 30 s × 2ⁿ, cap 10 min / `fail_fast` = 0 retries / `custom`)
  plus a generic async `retry` combinator with no HTTP knowledge. Wired into
  `anthropic_oauth` and `openai_client` (each fetch fn takes a
  `&BackoffPolicy`). Idempotent GETs retry on 429 + 5xx + transport; the
  token-rotating `refresh_access_token` POST retries **429-only** (a
  5xx/timeout retry could replay a consumed refresh token). `Retry-After`
  honored (delta-seconds), clamped to the policy cap; no jitter (single
  user). The one-shot CLI passes `fail_fast()` (never blocks an interactive
  invocation); the v0.2 watcher will pass `standard()`.

### Changed
- `balanze-cli`'s `build_snapshot` is now a one-line delegate to
  `snapshot_composer::compose`; the per-source fetch helpers moved into a
  `LiveSources` impl. Behavior-preserving — the integration suite + the new
  parity test pin it.

## [0.1.0] - 2026-05-15

v0.1 — **"Data"**: a complete, honest four-quadrant data layer as a CLI.
Distribution is source-only (`cargo install --git … balanze_cli`); no
binaries or GitHub Release artifacts (that's the v0.4 phase).

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
- Conventional Commits is now **enforced** by a blocking `commit-msg` lefthook hook (`<type>(scope)?: subject`; Merge/Revert/fixup!/squash! exempt). Keeps `git log` and squash-merge PR titles clean for the changelog.
- The CLI binary is **`balanze-cli`**, not `balanze`. `balanze` is reserved for the future src-tauri tray app to avoid a workspace build-artifact collision.
- Workspace `default-members = ["crates/*"]`: bare `cargo build`/`test`/`run` no longer build `src-tauri`. The `balanze-cli` deliverable now builds on Linux with **only a Rust toolchain** (+ a C compiler for `ring`) — no GTK/WebKit/`pkg-config` chain. The desktop app is the explicit `--workspace` / `bun run tauri dev` opt-in; its GUI system deps are documented in the README.
- `--sections` accepted as a bare top-level shortcut (peer of `--json`), so `balanze-cli --sections` works as the compact view's own footer and the docs advertise.
- Pre-tag cleanup (multi-agent review follow-up): de-flaked the integration test (deterministic `now`), sharpened the compact estimate label + legend, redacted a serde-error log path and the OAuth `Debug`/error-body surfaces, and closed several §6 validation-matrix test gaps.

### Known issues
- **Windows keychain backend silently no-ops** (`keyring 3.6.3`). Workaround: `BALANZE_OPENAI_KEY` env var takes precedence over keychain reads. Real fix lands with the `keyring` → `keyring-core` (v4) migration in **v0.3** (it rides with the settings UI that exercises the key-input box on both platforms).
- **Anthropic OAuth bearer expires every ~7–8 hours.** Currently surfaced as `AuthExpired`; re-run `claude login` and retry. Refresh-token flow is v0.1.1 work.
- **`extra_usage` block from OAuth has unclear semantics** — the `used_credits` field doesn't reconcile with claude.ai's "$ spent this month" UI. Suppressed in pretty CLI output; still in `--json` for diagnostics. The v0.3 Anthropic Console (HAR) investigation should resolve the units.
- **Anthropic API $ is an estimate, not real spend.** The official Usage & Cost API is enterprise/org-admin-gated (Phase-0 spike: NO-GO for the modal user). The JSONL-derived figure is the honest best-available signal and is labelled as such; a real-spend source is a v0.2+ research note contingent on enterprise access.

## Roadmap

Theme per phase: **Data → Liveness → UI → Distribution**.

- **v0.1 — Data** (this milestone): the four-quadrant CLI above.
- **v0.1.1 — released 2026-05-19** — proactive OAuth refresh-token flow; cap window anchored to OAuth's `resets_at` (was `now - 5h`); plus v0.2 Track B de-risk (`snapshot_composer` + `backoff`) shipped in the same tag.
- **v0.2 — Liveness** — next is Track C (Anthropic API $ honesty redesign), then Track D (statusline source); then the `watcher` crate (notify + debounce + `IncrementalParser` + safety poll) and `predictor` crate (EWMA + warm-up state machine on `window::WindowSummary`); `--watch`; `statusline`.
- **v0.3 — UI** — Tauri tray + popover; settings UI; `keyring` → `keyring-core` v4 migration (fixes the Windows keychain bug); degraded-state events; dashboard window; alerts; Anthropic Console cookie-paste source.
- **v0.4 — Distribution** — signed binaries (Windows cert, macOS notarization), Homebrew tap, WinGet manifest, Tauri auto-update.
- **v1+** — Ubuntu GNOME, cross-device sync, Android companion, hosted wallboard.

[Unreleased]: https://github.com/Oszkar/balanze/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/Oszkar/balanze/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Oszkar/balanze/releases/tag/v0.1.0
