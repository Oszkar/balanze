# Architecture

Balanze is an actor-model Tauri app. One thread owns canonical state; pollers feed it; the frontend reads it. Currency is `i64` micro-USD; HTTP is concentrated in two clients; secrets route through one keychain wrapper; the JSONL wire format is owned by one crate. The frontend ↔ backend surface is a fixed set of commands and events.

This doc is the architecture reference. Product scope lives in [`PRD.md`](PRD.md); operational discipline (validation gates, change control, test rules) lives in [`../AGENTS.md`](../AGENTS.md).

## Data flow

```
~/.claude/projects/**/*.jsonl ─notify+60s incr.──┐
~/.codex rollout files ────────60s safety poll───┤
OpenAI billing API ───────────5min poll──────────┤
Anthropic OAuth (fallback) ───5min poll──────────┤
Claude Code statusLine ───────push (per turn)────┤
Tauri commands ───────────────StateMsg::Query────┤
popover open / refresh_now ───StateMsg::Refresh──┤
                                                  ▼
                              ┌──────────────────────────────┐
                              │   StateCoordinator (actor)   │
                              │   owns: Snapshot, Settings,  │
                              │         AppHandle            │
                              └──────────────────────────────┘
                                          │
                       ┌──────────────────┼──────────────────┐
                       ▼                  ▼                  ▼
                emit("usage_updated") tray.set_icon    emit("degraded_state")
                                      tray.set_title   (Svelte UI)
                (Svelte UI)           (OS tray)
```

The statusLine push is the v0.2 live backbone for Anthropic quota when the user has an active Claude Code session; OAuth polling is the cold-start / no-session fallback (backoff'd, 429-tolerant).

## Crate map

```
balanze/
├── Cargo.toml, package.json, svelte.config.js, vite.config.js, tsconfig.json
├── docs/PRD.md                 product spec
├── src/                        Svelte 5 frontend - popover (grid/cards + settings views, IPC store, presentation helpers); v0.3.1
├── src-tauri/                  Tauri 2 app - gauge tray + live popover + settings + single-instance + filled TauriSink; v0.3.1
├── crates/
│   ├── claude_parser/          JSONL wire format: parse, walker, dedup, IncrementalParser, find_claude_projects_dir
│   ├── claude_cost/            pure JSONL → estimated $ vs vendored LiteLLM prices; infallible
│   ├── claude_statusline/      Claude Code statusLine payload + settings.json statusLine stanza (read/atomic-write)
│   ├── anthropic_oauth/        only HTTP client for /api/oauth/usage; reader/writer of ~/.claude/.credentials.json (+ read-only macOS login-Keychain credential)
│   ├── window/                 pure rolling-window math (5h + 30m burn + per-model) and pace (used vs elapsed)
│   ├── openai_client/          only HTTP client for /v1/organization/costs
│   ├── codex_local/            only reader of ~/.codex/; latest rate_limits.primary
│   ├── snapshot_composer/      single source-orchestration policy; CLI and watcher both run compose()
│   ├── state_coordinator/      actor: owns Snapshot; bounded-mpsc StateMsg loop; Sink-notified
│   ├── watcher/                only importer of notify; spawns jsonl/statusline/openai_poll/safety/oauth_poll tasks
│   ├── backoff/                pure exponential-backoff policy + generic async retry combinator
│   ├── keychain/               only importer of `keyring-core`
│   ├── settings/               owns settings.json (atomic, schema-versioned, non-secrets only)
│   └── balanze_cli/            CLI glue; composes the crates into a Snapshot
└── .github/workflows/          ci.yml + release.yml
```

## Boundaries

Strict layering — agents must respect these.

1. **`claude_parser` owns the JSONL wire format.** Other crates consume `UsageEvent` only; field names and line-format quirks do not leak.
2. **`window`, `claude_cost` are pure functions.** No I/O, no `tokio::spawn`, no logging above `debug`. `claude_cost` is infallible — unknown models route to `skipped_models`.
3. **`anthropic_oauth` + `openai_client` are the only HTTP clients.** Other crates do not issue HTTP requests; the glue crates (`balanze_cli`, `watcher`) may construct a `reqwest::Client` to inject into these two, but all request/retry/redaction logic lives here. `anthropic_oauth` is additionally the only reader and writer of `~/.claude/.credentials.json` (atomic tmp+fsync+rename, perms-preserving, OAuth fields only, never regresses a concurrently-newer on-disk token). On recent macOS, where Claude Code keeps that credential in the login Keychain rather than a file, `anthropic_oauth` reads that entry **read-only** (via `/usr/bin/security`, not `keyring`, so boundary #5 holds) and never refreshes or writes a credential it does not own; an expired Keychain token surfaces `CredentialExpiredReadOnly` (re-run `claude login`).
4. **`watcher` owns `notify` + the debounce + the 60 s safety poll.** Other crates do not import `notify`. `Watcher::spawn(handle, &Settings) -> Vec<(&'static str, JoinHandle<...>)>` returns one labeled handle per task; default-spawned (3): `jsonl` (notify + 300 ms debounce + a 60 s fallback; reads new bytes only via a per-file byte cursor, `claude_parser::IncrementalParser`, so neither path full-reparses after launch per §3.1), `statusline`, `safety` (a 60 s re-read of statusline + Codex - it no longer scans JSONL, the `jsonl` task's 60 s fallback covers that; its Codex scan gated on `providers.codex_enabled`); conditionally (2): `openai_poll` when `providers.openai_enabled` or a `BALANZE_OPENAI_KEY` env override is set, and `oauth_poll` when `providers.anthropic_enabled`. The caller (`balanze-cli --watch`, or the Tauri host) supervises under `tokio::select!`. `Ok(Ok(()))` is graceful (e.g. no OpenAI key) and must not trigger teardown. The CLI treats an `Ok(Err(_))` / panic from any task as fatal and exits with a restart hint. The Tauri host self-heals instead: it re-spawns the whole set on a settings change (provider toggles apply live) AND on an unexpected task death, where it also emits a `degraded_state` for the affected source (mapped by `watcher::source_for_label`) and backs off (exponential, 1 s to a 60 s cap) so a persistent failure stays visibly degraded rather than silently frozen. Tasks feed the coordinator directly via `StateCoordinatorHandle::send(StateMsg::Update(...))`.
5. **`keychain` is the only caller of `keyring-core` (plus the OS-native store crates).** All secret reads/writes route through this crate.
6. **`settings` owns `settings.json`.** Atomic writes (tmp + rename). No other crate reads or writes this file.
7. **`state_coordinator` is the only writer of the in-memory `Snapshot` and the only driver of tray repaints (via the `Sink` trait).** Pollers send `StateMsg::Update(SourcePartial)`; the coordinator applies the partial and notifies the `Sink`. The concrete `TauriSink` (in `src-tauri`) is the only caller of OS tray APIs: it emits `usage_updated` and repaints, deduping by `(ColorBucket, title)` (its own `last_painted`). A `StateMsg::Refresh` (popover open / `refresh_now`) re-notifies the `Sink` with current state so it can repaint - it never re-fetches, and nothing outside the `Sink` touches OS tray state. The `ClaudeJsonl` partial carries **raw deduped events**, not a finished summary: the coordinator derives the window + API-rate cost via `state_coordinator::summarize_jsonl`, anchoring the rolling window to the latest OAuth `resets_at`. It caches the events, so a subsequent OAuth update (a new reset) re-anchors them — this is what keeps the live `--watch` window identical to the one-shot CLI. It also handles `StateMsg::SettingsChanged`: on a provider toggle it resets that provider's cell to "not configured" (so disabled providers don't linger with stale data) and repaints; the watcher re-spawn (boundary #4) stops/starts the actual polling.
8. **`src-tauri` and `balanze_cli` are glue, not logic.** Both turn backend crates into a `Snapshot`, by two routes: the **one-shot** path (`balanze-cli status`) runs `snapshot_composer::compose`; the **live** path (`balanze-cli --watch` today, the v0.3 Tauri host later) feeds incremental `StateMsg::Update` partials into the coordinator, which assembles the `Snapshot`. The JSONL → window+cost math is shared across both routes via `state_coordinator::summarize_jsonl` (called by `compose` and by the coordinator merge), so the live path cannot diverge from the one-shot path on the window anchor. Identical inputs ⇒ identical `Snapshot` cells (parity-tested in `integration_4quadrant::compose_parity_against_fixtures`).
9. **Frontend ↔ backend goes through the fixed IPC contract.** Commands: `get_snapshot`, `get_history`, `refresh_now`, `hide_window`, `set_api_key`, `has_api_key`, `clear_api_key`, `get_settings`, `set_settings`, `get_statusline_status`, `set_statusline_wired`. Events: `usage_updated`, `degraded_state`. Adding to this surface needs a design-doc update first.
10. **Currency math uses `i64` micro-USD.** Convert to `f64` only at the display boundary. Every provider's money is converted to i64 micro-USD at its own ingest/parse boundary (Anthropic `extra_usage` cents, OpenAI Admin Costs dollars), so the `Snapshot` holds no `f64` money. `claude_cost` keeps per-token prices in i64 nano-USD with i128 intermediates and saturates at `i64::MAX`.
11. **`codex_local` knows the Codex rollout-JSONL format and is the only reader of `~/.codex/`.** Honors `CODEX_CONFIG_DIR`. Other crates consume `CodexQuotaSnapshot`.
12. **`claude_statusline` owns the Claude Code statusLine wire format and the statusLine stanza in Claude's `settings.json`** (read + atomic write, idempotent, no-clobber — mirrors boundary #3). Also owns the `<ProjectDirs.data>/statusline.snapshot.json` inter-process bridge: `balanze-cli statusline` writes atomically; `watcher::tasks::statusline` reads on notify.

## IPC contract

Frontend ↔ backend, via Tauri commands and events only. Commands return `Result<T, String>` (`anyhow::Error::to_string()`).

| Direction | Name | Purpose |
|---|---|---|
| Command | `get_snapshot` | Current `Snapshot`. |
| Command | `get_history` | Recent rolling-window-sized history. |
| Command | `refresh_now` | Trigger an immediate poll/refresh. |
| Command | `hide_window` | Hide the popover (ESC-to-dismiss); window manipulation stays in Rust. |
| Command | `set_api_key` / `has_api_key` / `clear_api_key` | Store / probe-presence / remove the user-supplied OpenAI key in the keychain (never returns the value). |
| Command | `get_settings` / `set_settings` | Non-secret config (settings.json shape). `set_settings` live-applies provider toggles. |
| Command | `get_statusline_status` / `set_statusline_wired` | Read / wire-unwire the Balanze `statusLine` in Claude Code's `settings.json` (no-clobber). |
| Event | `usage_updated` | New `Snapshot` available. |
| Event | `degraded_state` | A source is stale / errored; surface visually. |

**Status:** implemented: `get_snapshot`, `refresh_now`, `hide_window`, `set_api_key`, `has_api_key`, `clear_api_key`, `get_settings`, `set_settings`, `get_statusline_status`, `set_statusline_wired`, `usage_updated`, `degraded_state`. Everything except `get_snapshot` / `refresh_now` is a synchronous command (Tauri runs them off the async runtime). The key commands touch only the keychain (the user-supplied OpenAI key; Anthropic uses Claude Code's OAuth) and never return the value; `set_statusline_wired` delegates to `claude_statusline` and is no-clobber. `set_settings` / `set_api_key` / `clear_api_key` live-apply: they send `StateMsg::SettingsChanged` (coordinator clears disabled cells) and signal the host to re-spawn the watcher, so provider toggles take effect without a restart. Planned: `get_history` (v0.6 dashboard; the durable SQLite history it reads lands earlier in v0.4.1). `refresh_now` today re-emits the current snapshot to the sink (repaint + catch-up); an on-demand provider re-poll is a later follow-up.

The CLI `--json` schema is the same `Snapshot` rendered through a presentation DTO (`crates/balanze_cli/src/json_output.rs`): the document carries a top-level `schema_version` (currently `1`) so a consumer can detect a breaking change, and every money cell is `{ value_micro_usd: i64, source, confidence, details }` - OpenAI Admin Costs included, converted to i64 micro-USD at the `openai_client` parse boundary so no money cell is `f64` - so consumers tell `jsonl_list_price` / `estimate` apart from `openai_admin_costs` / `real` and `extra_usage_billed` / `real` from the wire shape. Two extra cells (v0.2): `claude_statusline` carries the live `StatuslineFilePayload` envelope (Claude Code's session estimate — a distinct cost tier, no money normalization); `.pace` carries a per-window array (`key`, `used_fraction`, `elapsed_fraction`, `ratio`) derived from the OAuth cadence bars — used % vs elapsed % of each quota window (5h, 7d) plus their ratio, computed by `window::pace`; `ratio` is null right after a window reset. `--watch --json` reuses this DTO, one JSON document per line. Identifiers (`org_uuid`, Codex `session_id`) are redacted unless `-v`.

## Errors and degraded state

- App-level results use `anyhow::Result<T>`. No `.unwrap()` outside tests.
- `thiserror` enums live only in `claude_parser` — variants include `FileMissing`, `IoError`, `SchemaDrift { line, message }` so the StateCoordinator can pattern-match and set the right `DegradedState`.
- Long-running tasks (file watcher, polling, the coordinator itself) are supervised with a retained `JoinHandle` + `tokio::select!`. A coordinator exit/panic is fatal on both hosts (the CLI returns; the Tauri host calls `AppHandle::exit` on a genuine panic, but not on a shutdown abort). A watcher-task `Ok(Err)` / panic is fatal in the CLI (`--watch` exits with a restart hint); the Tauri host surfaces a `degraded_state` for the affected source and re-spawns the set with bounded backoff (self-heal, not silent freeze).
- External I/O retries via `backoff` (`standard` = 30 s × 2ⁿ cap 10 min; `fail_fast` = 0). Idempotent GETs retry on 429 + 5xx + transport; the token-rotating refresh POST retries 429-only (a replayed consumed refresh token strands the user).
- IPC errors surface to the UI as a `degraded_state` event; the tray icon shows a warning dot.
