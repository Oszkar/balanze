# Changelog

All notable changes to Balanze are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/spec/v2.0.0.html). Pre-1.0 - minor bumps may break; patch bumps are fixes only.

## [Unreleased]

### Changed
- **Local source ingestion follows file replacement** - Claude JSONL truncations, atomic rewrites, and deletions now replace or remove only that file's owned events before cross-file deduplication; partial UTF-8 writes wait for a terminating newline, and Codex session traversal stays inside its configured root without following directory cycles.
- **Claude credentials are read-only on every platform** - Balanze no longer exchanges Claude Code's rotating refresh token or writes file-backed credentials. Expired or rejected credentials now consistently ask the user to run `claude login`; a future explicit file-refresh opt-in may be added separately.
- **Atomic-write durability errors are honest** - Unix parent-directory fsync failures are returned after rename instead of being silently treated as a fully durable success.

## [0.4.3] - Codex maturity - 2026-07-10

Codex gets first-class treatment: both rolling windows (5-hour and weekly) now surface across the tray, popover, cards, CLI, and statusline, alongside a parser-reliability fix, honest file logging, and a quieter macOS Keychain path.

### Added
- **Codex 5h + weekly windows everywhere** - all surfaces now show Codex's both 5h and weekly usage.
- **File logging + `BALANZE_LOG`** - both binaries now honor `BALANZE_LOG` (same syntax as `RUST_LOG`; an invalid value warns and falls back to `info` instead of silently reverting) and write a daily-rotating log kept 3 days under the data directory's `logs/`, so a windowless launch (a double-click on Windows) still leaves a trail.
- **User guide** - a first-run-to-statusline walkthrough (`docs/GUIDE.md`), plus a tightened README and PRD.

### Fixed
- **Popover shows real over-limit overage instead of "none"** - the same bug the compact CLI view already fixed in v0.4.1. All other views now detect the breach from `used >= limit` and show `$X/$Y over limit (real)`, and CLI can no longer diverge.
- **Parser no longer stalls on a bad JSONL line** - the reader now skips any bad lines if present, while keeps the good events and advances the cursor past the batch.
- **macOS Keychain no longer re-prompts every poll** - the watcher was re-reading Claude Code's credential on every 5-minute tick, re-triggering the macOS access prompt each time.

## [0.4.2] - Statusline maturity - 2026-07-07

The Claude Code statusline becomes a cross-provider, installable command - it replaces an existing statusline with backup/restore consent and enforces OpenAI Admin Costs politeness via an on-disk cache.

### Added
- **Cross-provider statusline** - merges Claude Code subscription quota with OpenAI costs (real billed spend) and Codex CLI quota in a single prompt line.
- **`statusline_render` crate + style engine** - configurable display segments, ANSI color/threshold scaling, and dark/light palettes.
- **Self-compose fallback path** - a headless, no-app fallback that reads Codex files locally and fetches OpenAI costs directly behind a short 3s timeout.
- **OpenAI cost cache** - enforces the 5-minute OpenAI politeness gate machine-wide via a 300s TTL file cache, keyed securely by FNV-1a key fingerprints.
- **Replace and restore with consent** - setup replaces an existing statusline in Claude Code's `settings.json` with user consent, saving a backup to restore it cleanly at any time.
- **Generalized rate limits** - parses arbitrary rate-limit window arrays dynamically, preparing for future Anthropic rate-limit windows.
- **Sonnet 5 pricing** - vendored the Sonnet 5 price table into the list-price leverage estimate.

### Changed
- **Unified quota coloring** - one green / yellow / orange / red scale at 50 / 75 / 90 now spans the tray, popover, CLI, and statusline (the statusline previously stayed neutral until 70). Every surface classifies the rounded displayed value, so a shown percentage and its color always agree at a cutoff.

### Fixed
- **Safe settings modification** - stopped settings save paths from clobbering a malformed configuration, bailing with a hint instead of defaulting.
- **Stale statusline guard** - checked `captured_at` freshness and clock skew on statusline snapshots in the coordinator and UI, falling back to live OAuth if stale.
- **Pace freshness** - kept window pace metrics fresh across poller ticks, refresh requests, and settings transitions.
- **Windows Vite 8 deadlock** - forced the dev server to bind `127.0.0.1` instead of `localhost`, fixing a Node.js IPv6 module-runner hang on Windows.
- **UI synchronization** - fixed watcher-to-store update delivery and popover store synchronization during live refresh events.
- **Tray title and tooltip name the worst window** - the icon color, the macOS menu-bar title, and the hover tooltip now derive from one view, so a red icon can no longer sit beside a low number. The tooltip explains the color and shows connecting / unavailable states; the cryptic C / O labels are gone.

## [0.4.1] - CLI maturity - 2026-06-27

CLI maturity: `balanze-cli` becomes a first-class, scriptable surface. `status` is now colored, and `doctor` / `export` / `watch` (TUI) / `completions` land alongside an exit-code taxonomy.

### Added
- **clap-derive CLI surface** - the whole command tree moved to clap (new `cli` module); bare `balanze-cli` still defaults to `status`. New global flags `-v`/`--verbose`, `--quiet`, `--no-color`, `--strict`, `--version`, plus auto-generated `--help`/`-h`.
- **Colored `status`** - the compact 4-quadrant matrix is colored on a TTY, honoring `NO_COLOR` / `--no-color` / non-TTY; a shared `present` module replicates the tray's 50 / 90 color-bucket thresholds so the CLI and tray cannot diverge.
- **`doctor [--offline]`** - per-integration diagnostics; `--offline` skips the network validation of the OpenAI key. `setup`'s readiness summary reuses the same probes (one keychain read per run).
- **Exit-code taxonomy** - `0` ok / `1` other / `2` usage / `3` auth / `4` network / `5` degraded-under-`--strict`; `main` classifies the outcome once and `doctor` shares the taxonomy.
- **`export [-o <file>]`** - stateless CSV of usage history, re-derived from JSONL each run (nothing persisted): Claude `(day, model)` rows with token counts + a list-price *leverage* column (`jsonl_list_price`, estimate) and OpenAI current-month *billed* rows (`openai_admin_costs`, real) in provenance-separated columns that are never summed.
- **`watch` TUI** - on an interactive terminal `balanze-cli watch` draws a bounded ratatui/crossterm TUI (`q` / `Esc` / `Ctrl-C` to quit; `r` refreshes).
- **Shell completions + man page** - `completions <shell>` (bash, zsh, fish, powershell, elvish) and a hidden `man` subcommand print to stdout; a `build.rs` also renders both into `OUT_DIR` for packaging.

### Changed
- **Breaking: the CLI moved from bare flags to subcommands.** `balanze-cli --json` is now `balanze-cli status --json`; `balanze-cli --sections` is now `balanze-cli status --sections`; `balanze-cli --watch` is now `balanze-cli watch`. Bare `balanze-cli` (no subcommand) still defaults to `status`. `-v` is now a global flag (was status-local).
- **`--quiet` is scriptable** - it suppresses the human-readable status matrix (but NOT `--json`, which is data) and trims `doctor` to WARN/FAIL lines only.

### Fixed
- **Over-limit pay-as-you-go overage now reads as real billed money.** Past the monthly cap, Anthropic flips the `extra_usage` block to `is_enabled=false` while keeping the real billed amount (and clamps utilization to 100%). The compact matrix, `--sections`, and the `watch` TUI all keyed on `is_enabled` alone and mislabeled it "not configured" / "not available" / "not enabled" - hiding real spend exactly when it was highest. Now detected from `used >= limit` and shown as `$X/$Y over limit (real)`.
- **statusLine UTF-8 BOM tolerance** - `balanze-cli statusline` strips a leading UTF-8 BOM before parsing, so a BOM-prefixed payload (e.g. piped from PowerShell) no longer reads as a parse error.

## [0.4.0] - UI maturity - 2026-06-23

The popover stops reading like a scaffold. New type system and a design-token set, real empty / loading / error states across every cell, a first-run welcome, and parity between the 2 views.

### Added
- **Design-token foundation + machined tiles** - a self-hosted type system, design tokens for spacing / type scale / color, machined-depth tiles, and content-hugging popover height.
- **Real cell states across the matrix:**
  - **Actionable empty states** - not-detected, add-OpenAI, and retry prompts replace silently blank cells.
  - **`SourceUnavailable` coordinator state** - a not-configured source is now distinct from an errored one.
  - **Plain-language cold-start cell** - a connecting source reads in plain language, no jargon tooltip.
  - **Neutral tray gauge color for the no-data state** - no misleading color before there is data to show.
- **First-run welcome** - first launch auto-opens the popover and fires a notification so the tray icon is discoverable.
- **Cards view reaches parity with the Grid** - Anthropic state + burn indicator, the OpenAI column (connect / dismiss / error), quota-state gating, and aligned source badges / tooltips / labels.
- **OpenAI key validation on save** - the key is validated before it lands, with inline admin-key help and surfaced link-open failures.
- **Dev-only states gallery** - a standalone CSR harness rendering every popover screen for visual review and snapshotting.

### Changed
- **Provenance badges trimmed to the billed-money signal** - badges now mark only real billed spend, cutting noise from cells where provenance is unambiguous.

### Fixed
- **Accessibility, motion, and currency-format polish** - reduced-motion handling, focus / a11y on the column controls, and consistent currency formatting.

## [0.3.1] - Settings & trust - 2026-06-20

The Settings UI lands - keys, live provider toggles, statusLine wiring - alongside the trust pass: real keychain persistence on Windows, the macOS Keychain OAuth read, and honest Codex staleness.

### Added
- **Settings UI in the popover** (gear icon).
  - Manage OpenAI Admin key straight on the UI (Set / Replace / Remove)
  - Toggle each provider live - a disabled provider's cell clears instead of going stale
  - Wire/unwire Claude Code's `statusLine` on the UI, bringing the desktop app to parity with `balanze-cli setup`.
- **Degraded-state banner** - a stale or errored source shows a visible warning naming the affected sources instead of silently blanking the cell.

### Changed
- **Toolchain pinned to a single source** - Rust 1.94.0 via `rust-toolchain.toml`, Bun 1.3.13 via `packageManager`.
- **OpenAI money is `i64` micro-USD end to end**, with snapshot schema versioning and uniform serde-error redaction at the JSON-parse boundaries.
- **Watcher reads Claude JSONL incrementally** (per-file byte cursor) instead of full-reparsing on every change - no CPU spike during an active Claude session.
- **Vendored Claude price table** refreshed to LiteLLM `1ccc1e5` (2026-06-18); adds `claude-opus-4-8` and `claude-fable-5` so the subscription-leverage estimate prices current-default-model usage instead of dropping it into `skipped_models`.

### Fixed
- **Windows keychain persistence** - migrated to `keyring-core` plus the OS-native store crates, registered once at startup via `keychain::init_default_store`. Closes the v0.1 known issue.
- **macOS Anthropic quota** - recent Claude Code on macOS keeps its OAuth credential in the login Keychain. Balanze now reads it as a read-only source; an expired token surfaces "re-run `claude login`" rather than a refresh attempt (Claude Code keeps sole ownership of token rotation).
- **Codex quota honesty** - when the rollout's window has already reset, the cell degrades `✓` to a `⚠ stale` marker instead of a confidently-wrong used %; short windows now render human units (a 5-hour window reads `5h`, not `0d`). An absent `~/.codex/sessions` (Codex not installed) is treated as a quiet not-configured state, not a `codex_quota_error` on every tick.
- **Popover** - a single refresh control in the header, a 1px adaptive border so edges show over a same-colored background, ESC to dismiss. It re-pulls the snapshot via `get_snapshot` on every open, so it self-heals if the `usage_updated` listener is ever orphaned.
- **Watcher reliability** - pollers build the HTTP client per tick and retry instead of freezing a cell on a one-time build failure; dead watcher tasks self-heal and a coordinator panic exits the Tauri host cleanly; request timeouts are bounded.
- **Statusline drift tolerance** - a partial/incomplete `rate_limits` window degrades to `None` instead of erroring the whole payload; an out-of-range `resets_at` window is dropped with a warning instead of being rewritten to the Unix epoch.

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
- **Proactive Anthropic OAuth refresh** - `refresh_access_token` + atomic anti-clobber `write_back`; pre-flight refresh + one 401-retry. Bearer no longer hard-fails every ~7-8 h.
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


[Unreleased]: https://github.com/Oszkar/balanze/compare/v0.4.3...HEAD
[0.4.2]: https://github.com/Oszkar/balanze/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/Oszkar/balanze/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/Oszkar/balanze/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/Oszkar/balanze/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/Oszkar/balanze/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/Oszkar/balanze/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Oszkar/balanze/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/Oszkar/balanze/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Oszkar/balanze/releases/tag/v0.1.0
