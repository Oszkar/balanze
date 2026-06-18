# Changelog

All notable changes to Balanze are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/spec/v2.0.0.html). Pre-1.0 - minor bumps may break; patch bumps are fixes only.

## [Unreleased]

### Changed
- Toolchain unification: Rust pinned to 1.94.0 via `rust-toolchain.toml` (CI matches; MSRV stays 1.85), Bun pinned to 1.3.13 via `packageManager` + CI `bun-version-file`.
- PR titles are now CI-validated as Conventional Commits (`pr-title.yml`), matching the local `commit-msg` hook.
- CI installs Rust via `actions-rust-lang/setup-rust-toolchain@v1`, which reads `rust-toolchain.toml` - the toml is now the single pin source, and dependabot no longer proposes toolchain releases as action-tag bumps (the dtolnay action encoded the version in its ref).
- Vendored Claude price table refreshed to LiteLLM `1ccc1e5` (2026-06-18); adds `claude-opus-4-8` and `claude-fable-5` so the subscription-leverage estimate prices current-default-model usage instead of dropping it into `skipped_models`.

### Fixed
- Windows keychain persistence: migrated from the monolithic `keyring 3.x` (which silently no-op'd on Windows - `set_password` returned `Ok` but the credential never landed in Credential Manager) to `keyring-core` plus the OS-native store crates (`windows-native-keyring-store` / `apple-native-keyring-store`). Each binary registers the platform store once at startup via the new `keychain::init_default_store`. The real-keychain roundtrip smoke now passes on Windows.
- Codex quota cell is honest about staleness: when the rollout's primary window has already reset (`now > resets_at`), the compact view degrades the `✓` to a `⚠ ... stale` marker and the popover flags the cell, instead of showing a confidently-wrong used %. The compact window label also renders human units (a 5-hour window now reads `5h`, not `0d` - `300 / 1440` floored to zero).
- Popover re-pulls the snapshot on every open (on window-show) and the refresh button now fetches via `get_snapshot` directly, instead of depending on the `usage_updated` event. The popover is fresh on open and self-heals if the event listener is ever orphaned (e.g. a dev-server reload leaving usage frozen while the window clock kept ticking).
- Statusline windows with an out-of-range `resets_at` are dropped with a warning instead of being silently rewritten to the Unix epoch.
- Statusline parser tolerates partial window-shape drift: a present-but-incomplete `rate_limits.{five_hour,seven_day}` window (missing `used_percentage`/`resets_at`, e.g. a future field rename) degrades that one window to `None` - logged at `warn!` so the drift stays visible - instead of erroring the whole payload; the other window and the session cost survive. A present `null` or wrong-type field still surfaces as `SchemaDrift`.
- Watcher pollers survive a `reqwest` client-build failure: `openai_poll` / `oauth_poll` built the HTTP client once before the loop and exited cleanly on failure - silently freezing that cell until restart. They now build the client per tick, emit the error, and retry, so the degraded state stays visible and self-heals.

## [0.3.0] - UI: the popover PoC - 2026-06-10

The Tauri surface - the hero artifact. Gauge tray + glanceable popover, wired live to the v0.2 watcher spine.

### Added
- **Tauri popover + gauge tray icon.** Color-shifting ring gauge (RGBA rendered at runtime, repaint deduped by `(ColorBucket, title_text)`); hidden-on-launch, left-click toggles, blur hides.
- **Popover views.** Transposed matrix grid (providers as columns, quota/billed rows, pace tick inline on the usage bar) + a Cards density view; "Subscription leverage" box; burn number; source/confidence on hover; light/dark.
- **Live IPC**: commands `get_snapshot`, `refresh_now`; events `usage_updated`, `degraded_state`.
- **`TauriSink` filled** - real tray paint + emit (was the compile-only seam-check skeleton in v0.2). `tauri-plugin-single-instance` prevents double-launch.

## [0.2.0] - Liveness - 2026-06-03

The data updates itself. New live spine = statusline-push + JSONL `notify`; OAuth demoted to a backoff'd fallback poll.

### Added
- **`balanze-cli --watch`** - long-running refresh loop; `StdoutSink` (TTY redraw) + `JsonlSink` (one JSON doc/line); supervised under `tokio::select!`.
- **`watcher` crate** - the only `notify` importer; spawns `jsonl` / `statusline` / `openai_poll` / `safety` (+ `oauth_poll` when enabled), feeding the coordinator.
- **Claude Code statusLine integration** - `claude_statusline` crate + `balanze-cli statusline` (live 5h/7d quota + session cost, zero-auth); `setup` offers to wire it. Atomic `statusline.snapshot.json` is the IPC bridge the watcher reads.
- **Real pay-as-you-go overage surfaced** - `extra_usage` spent/limit/% (cents; reconciled against claude.ai).
- **Tagged-DTO `--json`** - every money cell is `{ value_micro_usd, source, confidence, details }`.
- **HTTP-date `Retry-After`** (IMF-fixdate + delta-seconds; past dates clamp to 0).
- **Criterion baselines** for the cost/parse hot paths (`compute_cost` / `summarize_window` / `incremental_parser`; local-only).

### Changed
- **Pace view replaces the EWMA predictor** (`Snapshot.pace`, `window::pace`); `predictor` crate retired. R1: the list-price estimate leaves the matrix cell → separate "Subscription leverage" line.
- `Settings::oauth_poll_interval_secs` (default 300; floor-clamped to 300 per §3.1).
- Anthropic API-$ estimate hard-labeled "subscription leverage, NOT billed".
- `set-openai-key` uses a masked TTY prompt / piped stdin (no longer positional argv).
- `--json` redacts `org_uuid` / Codex `session_id` unless `-v`.
- `release.yml` requires an explicit `v*.*.*` tag input.

### Fixed
- JSONL discovery unions every project root - dual-install machines no longer undercount.
- `--watch` anchors the JSONL window to the OAuth reset (CLI ≡ watcher) via a shared `summarize_jsonl` helper.
- Parse `extra_usage` when `utilization` is `null` at $0 usage (#56).

## [0.1.1] - 2026-05-19

OAuth keepalive + the v0.2 de-risk foundations (Tracks A + B), shipped in one tag.

### Added
- **Proactive Anthropic OAuth refresh** - `refresh_access_token` + atomic anti-clobber `write_back`; pre-flight refresh + one 401-retry. Bearer no longer hard-fails every ~7–8 h.
- Cap window anchored to OAuth's `resets_at` (was `now − 5h`), removing clock-drift error.
- **`snapshot_composer` crate** - one `compose()` shared by the CLI and the future watcher (parity-tested).
- **`backoff` crate** - exponential policy + async retry, wired into both HTTP clients (CLI fail-fast; watcher standard).

### Changed
- `anthropic_oauth` is now also a *writer* of `~/.claude/.credentials.json` (atomic, perms-preserving, OAuth fields only).

## [0.1.0] - 2026-05-15

v0.1 - **"Data"**: a complete, honest four-quadrant data layer as a CLI. Distribution is source-only (`cargo install --git ... balanze_cli`).

### Added
- **`balanze-cli`** - `status` (4-quadrant compact view) / `setup` / `set-openai-key` / `clear-openai-key` / `settings`; `status` takes `--sections` / `--json` / `-v`.
- **Four-quadrant matrix** - Anthropic quota % (OAuth) · Anthropic API $ estimate (JSONL × LiteLLM, labeled leverage) · OpenAI Codex quota % · OpenAI API $ real billed (Admin Costs).
- Crates: `claude_parser` (JSONL parse/dedup/incremental), `claude_cost` (pure estimate vs vendored prices), `anthropic_oauth`, `openai_client`, `codex_local`, `window`, `state_coordinator` (actor scaffold), `settings` (atomic), `keychain`.
- `balanze-cli setup` interactive auth wizard; a 4-test end-to-end integration suite; CI on Windows + macOS; Dependabot.
- README, AGENTS.md, SECURITY.md, MIT LICENSE.

### Changed
- Conventional Commits enforced by a blocking `commit-msg` hook.
- CLI binary is **`balanze-cli`** (`balanze` reserved for the tray app).
- `default-members = ["crates/*"]` - bare `cargo` skips `src-tauri`, so the CLI builds with no GUI stack.

### Known issues
- **Windows keychain backend silently no-ops** (`keyring 3.6.3`). Workaround: `BALANZE_OPENAI_KEY`. Real fix rides the settings UI (`keyring`→`keyring-core` v4, v0.3.1).
- Anthropic API $ is an *estimate*, not real spend (official Usage & Cost API is org-admin-gated - Phase-0 NO-GO).


[Unreleased]: https://github.com/Oszkar/balanze/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/Oszkar/balanze/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Oszkar/balanze/releases/tag/v0.1.0
[0.2.0]: https://github.com/Oszkar/balanze/releases/tag/v0.2.0
[0.3.0]: https://github.com/Oszkar/balanze/releases/tag/v0.3.0
