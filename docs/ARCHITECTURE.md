# Architecture

Balanze is an actor-model Tauri app. One thread owns canonical state; pollers feed it; the frontend reads it. Currency is `i64` micro-USD; HTTP is concentrated in two clients; secrets route through one keychain wrapper; the JSONL wire format is owned by one crate. The frontend ↔ backend surface is a fixed set of commands and events.

This doc is the architecture reference. Product scope lives in [`docs/prd.md`](docs/prd.md); operational discipline (validation gates, change control, test rules) lives in [`AGENTS.md`](AGENTS.md).

## Data flow

```
~/.claude/projects/**/*.jsonl ──notify+debounce──┐
                              ──60s safety poll──┤
OpenAI billing API ───────────5min poll──────────┤
Anthropic OAuth (fallback) ───5min poll──────────┤
Claude Code statusLine ───────push (per turn)────┤
Tauri commands ───────────────StateMsg::Query────┤
30s tray ticker ──────────────StateMsg::Refresh──┤
                                                  ▼
                              ┌──────────────────────────────┐
                              │   StateCoordinator (actor)   │
                              │   owns: Snapshot, Settings,  │
                              │         history, AppHandle   │
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
├── docs/prd.md                 product spec
├── src/                        Svelte 5 frontend (scaffold today; real UI is v0.3)
├── src-tauri/                  Tauri 2 app crate (scaffold tray + single-instance + compile-only TauriSink)
├── crates/
│   ├── claude_parser/          JSONL wire format: parse, walker, dedup, IncrementalParser, find_claude_projects_dir
│   ├── claude_cost/            pure JSONL → estimated $ vs vendored LiteLLM prices; infallible
│   ├── claude_statusline/      Claude Code statusLine payload + settings.json statusLine stanza (read/atomic-write)
│   ├── anthropic_oauth/        only HTTP client for /api/oauth/usage; only reader/writer of ~/.claude/.credentials.json
│   ├── window/                 pure rolling-window math (5h + 30m burn + per-model)
│   ├── predictor/              pure EWMA with Insufficient → Uncertain → Confident warm-up state machine
│   ├── openai_client/          only HTTP client for /v1/organization/costs
│   ├── codex_local/            only reader of ~/.codex/; latest rate_limits.primary
│   ├── snapshot_composer/      single source-orchestration policy; CLI and watcher both run compose()
│   ├── state_coordinator/      actor: owns Snapshot; bounded-mpsc StateMsg loop; Sink-notified
│   ├── watcher/                only importer of notify; spawns jsonl/statusline/openai_poll/safety/oauth_poll tasks
│   ├── backoff/                pure exponential-backoff policy + generic async retry combinator
│   ├── keychain/               only importer of `keyring`
│   ├── settings/               owns settings.json (atomic, schema-versioned, non-secrets only)
│   └── balanze_cli/            CLI glue; composes the crates into a Snapshot
└── .github/workflows/          ci.yml + release.yml
```

## Boundaries

Strict layering — agents must respect these.

1. **`claude_parser` owns the JSONL wire format.** Other crates consume `UsageEvent` only; field names and line-format quirks do not leak.
2. **`window`, `predictor`, `claude_cost` are pure functions.** No I/O, no `tokio::spawn`, no logging above `debug`. `claude_cost` is infallible — unknown models route to `skipped_models`.
3. **`anthropic_oauth` + `openai_client` are the only HTTP clients.** Other crates do not import `reqwest`. `anthropic_oauth` is additionally the only reader and writer of `~/.claude/.credentials.json` (atomic tmp+fsync+rename, perms-preserving, OAuth fields only, never regresses a concurrently-newer on-disk token).
4. **`watcher` owns `notify` + the debounce + the 60 s safety poll.** Other crates do not import `notify`. `Watcher::spawn(handle, &Settings) -> Vec<(&'static str, JoinHandle<...>)>` returns one labeled handle per task; default-spawned (4): `jsonl`, `statusline`, `openai_poll`, `safety`; conditionally (1): `oauth_poll` when `providers.anthropic_enabled`. The caller (`balanze-cli --watch` today; v0.3 Tauri host later) supervises under `tokio::select!`. `Ok(Ok(()))` is graceful (e.g. no OpenAI key) and must not trigger teardown; only `Ok(Err(_))` or panic is fatal. Tasks feed the coordinator directly via `StateCoordinatorHandle::send(StateMsg::Update(...))`.
5. **`keychain` is the only caller of `keyring`.** All secret reads/writes route through this crate.
6. **`settings` owns `settings.json`.** Atomic writes (tmp + rename). No other crate reads or writes this file.
7. **`state_coordinator` is the only writer of the in-memory `Snapshot` and the only caller of Tauri tray APIs.** Pollers send `StateMsg::Update(SourcePartial)`; the coordinator merges, dedups by `last_painted`, emits, paints. The 30s tray ticker sends `StateMsg::Refresh` only — it never touches OS tray state itself.
8. **`src-tauri` and `balanze_cli` are glue, not logic.** Both compose backend crates into a `Snapshot`. The source-orchestration policy lives in `snapshot_composer::compose`; both entry-points run it. Identical inputs ⇒ identical `Snapshot`s (fixture-parity-tested).
9. **Frontend ↔ backend goes through the fixed IPC contract.** Commands: `get_snapshot`, `get_history`, `refresh_now`, `set_api_key`, `get_settings`, `set_settings`. Events: `usage_updated`, `degraded_state`. Adding to this surface needs a design-doc update first.
10. **Currency math uses `i64` micro-USD.** Convert to `f64` only at the display boundary. `claude_cost` keeps per-token prices in i64 nano-USD with i128 intermediates and saturates at `i64::MAX`.
11. **`codex_local` knows the Codex rollout-JSONL format and is the only reader of `~/.codex/`.** Honors `CODEX_CONFIG_DIR`. Other crates consume `CodexQuotaSnapshot`.
12. **`claude_statusline` owns the Claude Code statusLine wire format and the statusLine stanza in Claude's `settings.json`** (read + atomic write, idempotent, no-clobber — mirrors boundary #3). Also owns the `<ProjectDirs.data>/statusline.snapshot.json` inter-process bridge: `balanze-cli statusline` writes atomically; `watcher::tasks::statusline` reads on notify.

## IPC contract

Frontend ↔ backend, via Tauri commands and events only. Commands return `Result<T, String>` (`anyhow::Error::to_string()`).

| Direction | Name | Purpose |
|---|---|---|
| Command | `get_snapshot` | Current `Snapshot`. |
| Command | `get_history` | Recent rolling-window-sized history. |
| Command | `refresh_now` | Trigger an immediate poll/refresh. |
| Command | `set_api_key` | Store an `sk-admin-…` in the keychain. |
| Command | `get_settings` / `set_settings` | Non-secret config (settings.json shape). |
| Event | `usage_updated` | New `Snapshot` available. |
| Event | `degraded_state` | A source is stale / errored; surface visually. |

The CLI `--json` schema is the same `Snapshot` rendered through a presentation DTO (`crates/balanze_cli/src/json_output.rs`): every money cell is `{ value_micro_usd: i64, source, confidence, details }`, so consumers tell `jsonl_list_price` / `estimate` apart from `openai_admin_costs` / `real` and `extra_usage_billed` / `real` from the wire shape. Two extra cells (v0.2): `claude_statusline` carries the live `StatuslineFilePayload` envelope (Claude Code's session estimate — a distinct cost tier, no money normalization); `prediction` carries the predictor's `Insufficient` / `Uncertain` / `Confident` output. `--watch --json` reuses this DTO, one JSON document per line. Identifiers (`org_uuid`, Codex `session_id`) are redacted unless `-v`.

## Errors and degraded state

- App-level results use `anyhow::Result<T>`. No `.unwrap()` outside tests.
- `thiserror` enums live only in `claude_parser` — variants include `FileMissing`, `IoError`, `SchemaDrift { line, message }` so the StateCoordinator can pattern-match and set the right `DegradedState`.
- Long-running tasks (file watcher, polling, the coordinator itself) are supervised: spawn with retained `JoinHandle` + `tokio::select!`; a panic exits the process.
- External I/O retries via `backoff` (`standard` = 30 s × 2ⁿ cap 10 min; `fail_fast` = 0). Idempotent GETs retry on 429 + 5xx + transport; the token-rotating refresh POST retries 429-only (a replayed consumed refresh token strands the user).
- IPC errors surface to the UI as a `degraded_state` event; the tray icon shows a warning dot.
