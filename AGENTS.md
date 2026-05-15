# AGENTS.md — Operational Contract

Repo: `Oszkar/balanze` | Branch: `main`
Agents: Claude Code, Copilot, Gemini, Windsurf

## 0. Prime Rule: Clarify Before Acting

If requirements are ambiguous, incomplete, or conflicting:

1. Stop.
2. Ask targeted questions.
3. Propose 1–3 concrete interpretations.
4. Wait for confirmation **OR** proceed with the assumption stated explicitly,
   depending on impact.

**Calibration:**

- High-impact / hard-to-reverse (changes to the `UsageEvent` / `Snapshot` schema, the IPC contract between Rust and Svelte, the actor-model write boundary, the keychain wrapper, the predictor algorithm, anything touching secrets, schema changes to `settings.json` on disk, new Tauri capabilities) → **wait**.
- Low-impact / reversible (a clippy fix, adding a unit test, a CSS tweak, a doc reword, a new tray menu item, a log-level adjustment, a non-breaking refactor inside a single module) → **state the assumption and proceed**.
- When in doubt, wait.

## 1. System Context

Balanze = a local-first desktop tray utility that consolidates personal AI usage tracking (Claude subscription + Claude API + OpenAI API) into one normalized view. Tauri 2 + Rust + Svelte 5. Currently building **v0.1** for Windows 11 and macOS 15+; later phases add Ubuntu GNOME, cross-device sync, Android, and a hosted wallboard.

The product is explicitly a **side project**. Optimize for usefulness, low maintenance, and tight scope. Do not over-engineer for hypothetical scale or a team-of-many.

Out of scope: full enterprise cost allocation, multi-seat observability, browser automation as a headline feature, monetization, cloud sync (v0.1 is local-only; sync arrives in Phase 3).

Authoritative product spec: `docs/prd.md`. Architecture and step-by-step build sequence live in the design doc at `~/.gstack/projects/balanze/oszka-*-design-*.md` (not in repo because the project directory is single-user).

## 2. Engineering Principles

Apply at all times:

- **12-Factor App** — Config in env, stateless processes where possible, strict dev/prod parity.
- **DRY** — No duplication of domain logic. JSONL parsing happens in one crate; rolling-window math in one crate; etc.
- **YAGNI** — No speculative abstractions. The crate set is fixed and enumerated in the Repo Map (plus `predictor` + `watcher` still planned for v0.2); don't add a new crate because it "might be useful" — the Repo Map is the allowlist.
- **KISS** — Simplest viable implementation.
- **PoLP** — Least privilege always. Keychain reads happen in one crate; nothing else touches the `keyring` crate.
- **MVP Bias** — Solo developer; ship fast, document tech debt, do not gold-plate, do not architect for imaginary scale.

Correctness > Cleverness · Security > Convenience · Simplicity > Flexibility · Precision > Agreeability

### 2.1 Project conventions

| Concern | Convention |
|---|---|
| Rust edition | 2021 (Tauri 2 macros still lag on edition 2024 in May 2026 — pin until plugins-workspace catches up) |
| Rust MSRV | 1.77 (declared in workspace `Cargo.toml`); CI uses `dtolnay/rust-toolchain@stable` |
| Workspace | Single Cargo workspace at repo root; `src-tauri` + `crates/*` are members; workspace declares shared dependencies |
| Logging | `tracing` (not `log`); see §3.2 for level discipline |
| Async | `tokio` everywhere; never block the runtime; never hold a `tokio::sync::Mutex` across an `.await` of an unrelated lock |
| Errors | `anyhow::Result<T>` at app boundaries; `thiserror`-derived enums ONLY inside `claude_parser` where the StateCoordinator must distinguish `FileMissing` vs `SchemaDrift` for degraded state |
| Currency | `i64` micro-USD (1e-6 USD units) internally; convert to `f64` only at the display boundary. **Never** use `f64` for sums or threshold comparisons |
| Cap unit | Tokens for the Claude subscription rolling-window cap; micro-USD for OpenAI API cap. No synthetic-dollar pricing table on the cap math path |
| Frontend framework | Svelte **5 runes** (`$state`, `$derived`, `$props`). No Svelte 4 stores. SvelteKit with `adapter-static` in SPA mode (Tauri serves the static build) |
| Frontend bundler | Vite 8; TypeScript strict via `tsconfig.json` |
| Frontend env | `import.meta.env.VITE_*`; never read raw `process.env` |
| IPC contract | Frontend ↔ Backend: only via the commands + events enumerated in the design doc (`get_snapshot`, `get_history`, `refresh_now`, `set_api_key`, `get_settings`, `set_settings`; events `usage_updated`, `degraded_state`). Adding to this surface needs a doc update first |
| Filesystem paths | All persistent locations go through the `directories` crate (`ProjectDirs::from("me", "oszkar", "Balanze")`) — never hardcode `~/Library/...` or `%APPDATA%\...` inline |
| Code style | `cargo fmt` defaults own Rust line width (`max_width` 100, enforced by CI + the pre-commit hook) — don't hand-wrap code/comments to fight rustfmt. Markdown has **no** column cap (Repo Map / matrix rows are intentionally long; never reflow a doc to hit a width). `prettier` not configured (small frontend surface — match surrounding style) |
| Lint floor | `cargo clippy --workspace --all-targets -- -D warnings` passes; `bun run check` (svelte-check + tsc) passes |
| Commit messages | Conventional Commits: `<type>(scope)?(!)?: subject`. **Enforced** by a blocking `commit-msg` lefthook hook. Types: feat/fix/chore/docs/style/refactor/perf/test/build/ci/revert. Merge / Revert / fixup! / squash! / amend! are exempt. Squash-merge lands the PR title on `main`, so the PR title must match too |

## 3. Non-Negotiables

### 3.1 API politeness toward providers

There is no internal rate-limit gate — the only thing being rate-limited is *us* against Anthropic and OpenAI. Rules:

- OpenAI billing endpoints: poll at most every 5 minutes. Billing data updates infrequently; aggressive polling burns the user's rate quota for no gain.
- Anthropic Console (the v0.3 cookie-paste integration, if/when it lands): poll at most every 5 minutes. Respect any rate-limit headers; back off on 429 with exponential backoff (start 30s, cap 10 min).
- Claude JSONL: local file I/O, no rate limit, but read **incrementally** via per-file byte cursor (`HashMap<PathBuf, FileCursor { byte_pos, mtime, size }>`). Full reparse only on launch or `refresh_now()`. Detect atomic rewrites (size unchanged but mtime changed) and truncations (size < cursor).
- Tray icon repaint: 30s cadence, **deduped** by `(ColorBucket, title_text)`. Never call `tray.set_icon` if the bucket and title haven't changed since the last paint.

### 3.2 Error Handling & Logging

**Errors:**
- App-level results use `anyhow::Result<T>`. Bubble errors up; don't `.unwrap()` outside tests.
- `thiserror`-derived enums live only inside `crates/claude_parser/`. Variants must include `FileMissing`, `IoError`, `SchemaDrift { line: usize, message: String }` at minimum — the StateCoordinator pattern-matches on these to set the correct `DegradedState`.
- Tauri commands return `Result<T, String>` derived from `anyhow::Error::to_string()`. Frontend gets readable messages.
- Long-running tasks (file watcher, polling tasks, the StateCoordinator) must be supervised: spawn with a retained `JoinHandle` and a `tokio::select!` so a panic exits the process (the user's OS restarts the app on next launch, or systemd / launchd if we ever wire that up).
- External I/O (OpenAI billing, future Console scrape) must use exponential backoff. Never tight-loop on failure.
- Errors at IPC boundary: surface to UI as the `degraded_state` event so the tray icon shows a warning dot. Don't swallow `fetch` rejections.

**Logging (`tracing` crate):**

| Level | Use for |
|---|---|
| `error` | Operator must look — supervisor exits, persistent keychain failures, repeated parse errors after schema drift detection |
| `warn` | Recoverable but worth noticing — OpenAI 429 retry, watcher restart, atomic-rewrite cursor invalidation, dropped state-coordinator mpsc message |
| `info` | Normal lifecycle — app start, first JSONL parse complete, OpenAI tile populated, settings saved, window-reset transition observed |
| `debug` | Per-event detail — individual JSONL line parsed, state-coordinator message handled, predictor result computed |
| `trace` | Raw frame dumps; almost never enabled |

Default level: `INFO` for app modules, `WARN` for the parser (DEBUG-per-file JSONL parsing is gated behind env var `BALANZE_LOG=debug,balanze::claude_parser=trace` so heavy use doesn't blow through the 5MB rotation in hours). Logs rotate via `tracing-appender` (5 MB max, keep last 3). Don't log secrets (API keys, partial keys, hashes of keys) at any level. Periodic logs cap at one line per N minutes; never one-per-event at info level.

### 3.3 Legal context

Balanze reads:
- The user's own local Claude JSONL files at `~/.claude/projects/**/*.jsonl` (created by their own Claude Code installation — no scraping, no remote calls).
- OpenAI's documented billing API (`/v1/usage`, `/v1/dashboard/billing/*` — these are official surfaces; we use the same auth the user already configured for their account).
- Anthropic Console (the v0.3 cookie-paste integration) — if this requires scraping rather than a documented API, the data must be marked `DataSource::AnthropicConsoleScrape` with `Confidence::Estimated` per the PRD's transparency principle, and the user must be informed it may break.

This is for personal use only. Not affiliated with Anthropic or OpenAI. If a provider revokes access or their UI changes break a scrape, the right answer is to degrade gracefully (mark data stale) and never to circumvent their controls.

### 3.4 Secret hygiene

Secrets in scope: user-supplied API keys for OpenAI; plus read-only access to Claude Code's OAuth tokens at `~/.claude/.credentials.json` (and `~/.config/claude/.credentials.json` on newer Claude Code installs).

Rules for user-supplied keys (OpenAI):
- Keys live in the OS keychain via the `keyring` crate. They are **never** written to disk in plaintext, not even temporarily.
- Keys are **never** logged at any level. Periodic redacted form (`sk-…45 (len=51)`) is OK in a debug "show config" command if ever added; that's the only acceptable display surface outside the settings UI's masked input.
- `.env` is gitignored. The project doesn't load `.env` directly — secrets go through the OS keychain (`crates/keychain`), non-secret config goes through `directories::ProjectDirs` (`crates/settings`), and any user-tunable env var is documented in the CLI help (`BALANZE_OPENAI_KEY`, `BALANZE_LOG`). Add a `.env.example` only if a future feature actually requires `.env` loading.
- The settings UI's API-key input renders as `type="password"` (no clipboard side-effects, no autocomplete).

Rules for Claude OAuth tokens at `~/.claude/.credentials.json`:
- **`anthropic_oauth` is the only crate that reads and writes this file.** The only write is the refreshed-OAuth-token write-back via `write_back`, which is atomic (tmp + fsync + rename), preserves the existing file permissions, reuses Anthropic's file (never a new one), touches only the `claudeAiOauth` token fields, and never regresses a concurrently-newer on-disk token. We do not copy, persist, mirror, or back up the file's contents anywhere — not to settings, not to logs, not to cache, not to telemetry.
- The bearer token, refresh token, and any field of `claudeAiOauth.*` are treated as secrets identical to OpenAI keys: never logged at any level, never echoed in `--show-config` output, never displayed in the UI even partially.
- Writing a refreshed access token back to `~/.claude/.credentials.json` is implemented in v0.1.1: the write uses atomic tmp + fsync + rename and preserves the existing file permissions. We do not invent a new credentials file; we use Anthropic's.
- The file path itself (and its existence) IS loggable at INFO ("found credentials at <path>") — the contents are not.

General rules for both:
- Don't grow the secret surface without justification. New secrets require a clear rotation path and a `DegradedState` variant for "credential unavailable / expired" before they're added.
- If a user-supplied key leaks (commit / log share / screenshot), the user rotates it at the provider and pastes the new value into Settings. If a Claude OAuth token leaks, the user re-runs `claude login` to refresh both tokens. Balanze stores no audit trail of historical credentials.

## 4. Repo Map

```
balanze/
├── Cargo.toml                  workspace root: declares src-tauri + crates/*
├── Cargo.lock                  committed (binary crate workspace)
├── package.json + bun.lock     bun + Svelte 5 + TypeScript + Vite
├── svelte.config.js            SvelteKit adapter-static, SPA mode
├── vite.config.js
├── tsconfig.json
├── README.md
├── LICENSE                     MIT
├── .gitignore
├── docs/
│   └── prd.md                  product spec; phasing v0.1 → v1+
├── src/                        Svelte frontend
│   ├── app.html
│   ├── routes/
│   │   ├── +layout.ts
│   │   └── +page.svelte        currently: scaffold greet form (placeholder)
│   └── lib/                    (planned: stores, components)
├── src-tauri/                  Tauri 2 app crate
│   ├── Cargo.toml              uses workspace dependencies
│   ├── tauri.conf.json         tray icon "main"; window hidden + skipTaskbar
│   ├── build.rs                tauri-build
│   ├── icons/                  default scaffold assets (placeholder; planned: color states)
│   ├── capabilities/
│   │   └── default.json        Tauri capability declarations
│   └── src/
│       ├── main.rs             entrypoint shim
│       └── lib.rs              single_instance plugin + tray menu via app.tray_by_id("main")
├── crates/                     workspace members (one-line purpose; mechanism lives in the code + the boundaries below)
│   ├── claude_parser/          owns the Claude JSONL wire format: parse + walker + `dedup_events()` (by `(message_id, request_id)`) + `IncrementalParser` (byte-cursor, for the future watcher) + `find_claude_projects_dir()` (XDG + dual-path)
│   ├── claude_cost/            pure JSONL→**estimated**-$ synthesis vs a vendored LiteLLM price subset. Infallible (unknown models → `skipped_models`). i64 micro-USD / i128 intermediates / saturating. Output is subscription-leverage, never real spend
│   ├── anthropic_oauth/        only HTTP client for `GET /api/oauth/usage` AND only reader and writer of `~/.claude/.credentials.json`. Curated cadence labels + titlecased fallback. Pre-flight + on-401 refresh-token flow with atomic write-back shipped in v0.1.1
│   ├── window/                 pure rolling-window math: 5h main window + 30m burn rate + per-model breakdown (desc by tokens, ties name-asc for determinism)
│   ├── predictor/              (planned, v0.2) EWMA + warm-up state machine
│   ├── openai_client/          only HTTP client for the Admin Costs API (`GET /v1/organization/costs`, `sk-admin-…`). 401 → AuthInvalid; 403 → InsufficientScope (admin-key hint). Redacts `sk-` in error bodies
│   ├── codex_local/            **only reader of `~/.codex/`** (boundary #11). Latest `rate_limits.primary` → one `CodexQuotaSnapshot` (no stream/dedup in v0.1). Honors `CODEX_CONFIG_DIR`
│   ├── state_coordinator/      actor: owns the canonical `Snapshot`; bounded-mpsc `StateMsg` loop; notifies a `Sink`. Tauri-free (src-tauri later provides a `TauriSink`)
│   ├── watcher/                (planned, v0.2) notify + debounce + safety poll
│   ├── keychain/               the ONLY importer of `keyring` (boundary #5). **Known bug:** keyring 3.6.3 set→get fails on Windows; `BALANZE_OPENAI_KEY` env-var fallback; keyring-core (v4) migration is v0.3 — see §10a
│   ├── settings/               owns `settings.json` (boundary #6): serde + atomic write, schema-versioned, non-secrets only
│   └── balanze_cli/            glue entry-point composing the crates into a `Snapshot` (mirrors what src-tauri will do). Binary name `balanze-cli`; `balanze` is reserved for the src-tauri tray app (artifact-name collision)
└── .github/
    └── workflows/
        ├── ci.yml              fmt + clippy + cargo test + svelte-check on Win + Mac
        └── release.yml         matrix build on tag v*.*.* via tauri-action

(No `target/`, `node_modules/`, `build/`, `.svelte-kit/` — all gitignored.
 The design doc at ~/.gstack/projects/balanze/ is single-user and not in the repo.)
```

### Architectural Boundaries

The system is an actor-model Tauri app. One thread of execution owns the state; pollers feed it; the frontend reads it. Diagrammatically:

```
~/.claude/projects/**/*.jsonl ──notify+debounce──┐
                              ──60s safety poll──┤
OpenAI billing API ───────────5min poll──────────┤
Anthropic Console (if found) ─5min poll──────────┤
Tauri commands ───────────────StateMsg::Query────┤
30s tray ticker ──────────────StateMsg::Refresh──┤
                                                  │
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
                emit("degraded_state") tray.set_title  (Svelte UI)
                (Svelte UI)            (OS tray)
```

Strict layering — agents must respect:

1. **`claude_parser` knows the JSONL wire format; nothing else does.** Other crates consume `UsageEvent` structs only. Don't leak schema details (field names, optionality, line-format quirks) outside this crate.
2. **`window`, `predictor`, and `claude_cost` are pure functions.** No I/O, no `tokio::spawn`, no logging above `debug` level. Pure functions on event slices (`claude_cost` also takes a `&PriceTable`). `claude_cost` is infallible by design — unknown models route to `skipped_models`, never an error.
3. **`anthropic_oauth` and `openai_client` (and v0.2's Console source) are the only HTTP clients.** They expose typed `fetch_*` async functions that return `anyhow::Result<Partial>`. Other crates do not import `reqwest`. `anthropic_oauth` is additionally the only reader and writer of `~/.claude/.credentials.json` — see §3.4 for the secrets discipline this implies.
4. **`watcher` owns `notify` + the debounce + the 60s safety poll.** No other crate imports `notify`. The watcher exposes an `mpsc::Receiver<WatchEvent>` and that's it.
5. **`keychain` is the only caller of the `keyring` crate.** All secret reads and writes route through this crate's API. Settings UI commands invoke `keychain::set/get/delete`, not `keyring::*` directly.
6. **`settings` owns the `settings.json` file on disk.** Atomic writes (tmp + rename). No other crate reads or writes this file.
7. **`state_coordinator` is the ONLY writer of the in-memory Snapshot AND the ONLY caller of Tauri tray APIs (`tray.set_icon`, `tray.set_title`).** Pollers send `StateMsg::Update(SourcePartial)`; the coordinator merges, dedups by `last_painted`, emits to Tauri, paints the tray. The 30s tray ticker is a dumb `StateMsg::Refresh` sender — it never touches OS tray state itself.
8. **`src-tauri` and `balanze_cli` are the two glue entry-points.** Both compose the backend crates into a `Snapshot`. Neither contains business logic; if you're tempted to add a `#[tauri::command] fn compute_…` in `src-tauri` or a parallel computation in `balanze_cli`, that computation belongs in a crate. The two entry-points must produce identical `Snapshot`s for identical inputs; when they diverge, the underlying crate is wrong. **Open tech debt:** the source-orchestration policy (per-source fetch + error mapping + the "JSONL fail ⇒ both Anthropic cells None, no duplicate error" and "Codex absent ⇒ Ok(None)" rules) currently lives only in `balanze_cli::build_snapshot`. When v0.2's watcher/`--watch` (or v0.3's Tauri pollers) add a second composition path, this MUST be extracted into a shared crate so the two cannot silently diverge — see the `// TODO(v0.2):` marker on `build_snapshot`. Not extracted in v0.1 because the second consumer doesn't exist yet (YAGNI).
9. **Frontend talks to backend only via the IPC contract.** Commands: `get_snapshot`, `get_history`, `refresh_now`, `set_api_key`, `get_settings`, `set_settings`. Events: `usage_updated`, `degraded_state`. Adding a new command or event requires a design-doc update first.
10. **Currency math uses `i64` micro-USD.** Float arithmetic on money is a footgun (`0.1 + 0.2 != 0.3`, threshold comparisons flake near boundaries). Convert to `f64` ONLY at the display boundary. `claude_cost` additionally keeps per-token prices in i64 nano-USD with i128 intermediates and saturates at `i64::MAX`.
11. **`codex_local` knows the Codex rollout-JSONL format and is the only reader of `~/.codex/`.** Exposes `CodexQuotaSnapshot`; no other crate parses Codex session files or imports its schema. Analogous to boundary #1 for `claude_parser`. Honors the `CODEX_CONFIG_DIR` override.

## 5. Quick Start for Agents

### Every task

1. Identify the correct crate / layer (see Repo Map above).
2. List invariants affected by your change (which boundary in §4, which conventions in §2.1).
3. Implement the smallest safe diff that fixes the root cause.
4. Validate (see §6).
5. If ambiguous → ask targeted questions early.
6. Use context7 MCP for looking up Tauri / Svelte / crate documentation and verifying external library function signatures, especially Tauri 2 APIs (the v1 → v2 migration created a lot of stale docs in the wild).

Avoid drive-by refactors.

### Additional information

- `docs/prd.md` — product spec and phasing.
- The design doc at `~/.gstack/projects/balanze/oszka-*-design-*.md` — architecture, IPC contract, state coordination diagram, predictor state machine, test strategy, build sequence. This is the load-bearing document; if a section in this AGENTS.md is missing detail, check there.
- The backend data layer is shipped — see the Repo Map for the crate set. The Tauri frontend is still scaffold only (greet form + tray menu). As the planned crates land (`predictor`, `watcher`) this AGENTS.md grows with them — each adds a Repo Map line and may add a Validation Matrix row.

### Local dev (for agents that can run commands)

```bash
# First-time setup:
bun install

# Run the desktop app (compiles Rust + serves Svelte + opens window):
bun run tauri dev

# Subsystem checks (fast feedback):
cargo check --workspace          # type-check all crates
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bun run check                    # svelte-check + tsc

# Release build (validates bundling):
bun run tauri build              # produces MSI/NSIS on Win, DMG/app on Mac
```

Hot-reload: `bun run tauri dev` hot-reloads the Svelte frontend on save. The Rust backend does **not** — restart manually (`Ctrl-C` then re-run). For tight Rust iteration use `cargo watch -x 'check --workspace'` in a side terminal.

**`default-members = ["crates/*"]`:** bare `cargo build`/`test`/`run` (no flags) skip `src-tauri` — that's deliberate, so the CLI builds with zero GUI/system libs (critical on Linux: `src-tauri` drags in GTK/WebKit; the CLI must never require it). Use `--workspace` (or `bun run tauri dev`) to include `src-tauri`; on Linux that needs the GUI dev packages (README → "Building the desktop app"). All gates here pass `--workspace`/`-p` explicitly, so they're unaffected — only the no-flag path changed.

## 6. Validation Matrix

Before claiming work is done:

| Change touches | Required gates |
|---|---|
| Any `**/*.rs` in workspace | `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo fmt --all -- --check` |
| Any crate's logic | That crate's own tests stay green **and** you add/extend a test for the specific invariant you changed. For pure crates (`window`, `claude_cost`, future `predictor`): write the test **before** the impl change. Don't weaken an assertion to make it pass — see §7. |
| `crates/claude_parser/**` | + the documented failure modes still pass (file missing / partial final line / schema drift / empty / permission denied) + a real-data smoke (`cargo run -p claude_parser --example claude_parser_smoke`) |
| `crates/keychain/**` | + run the `#[ignore]`'d real-keychain smoke **manually on each OS before tagging a release** — cross-OS keychain in CI is unreliable, so CI green ≠ keychain works |
| `crates/{anthropic_oauth,openai_client}/**` | + the wiremock suite (the only thing exercising the HTTP layer); manual check against the real endpoint/credentials on the dev machine |
| `src-tauri/src/**/*.rs` | + `bun run tauri dev` smoke (tray icon appears, click opens window, Quit exits cleanly) |
| `src-tauri/tauri.conf.json` | + `bun run tauri build --no-bundle` succeeds (validates config without the slow bundle) |
| `src/**/*.{svelte,ts}` | `bun run check`; visually verify in `bun run tauri dev` |
| Workspace `Cargo.toml` | `cargo check --workspace`; `cargo tree --workspace --duplicates` if you added a dep |
| `.github/workflows/*.yml` | YAML lints clean; for `release.yml`, trigger via `workflow_dispatch` after merge before tagging |
| `docs/prd.md`, `README.md`, `AGENTS.md` | Cross-reference internal links; check snippets still match current crate/command names; verify `prd.md` Phasing matches reality if scope shifted |

`cargo clippy -D warnings` is **strict**. The repo passes clippy clean from the scaffold and CI enforces it. Do not add `#[allow]` to silence lints unless there's a documented technical reason in a comment immediately above.

### If unable to run

State the exact command and request output from a human. Don't claim the work is done.

## 7. Test Discipline

- Run the smallest meaningful set first; expand based on risk.
- Do not weaken assertions or modify tests without invariant reasoning.
- If you find unrelated failures, call them out separately with evidence.
- Tests encode invariants — treat them accordingly. Tests should be strict.
- **When you change a load-bearing pure function, add a test before changing the implementation.** Especially true for `window`, `predictor`, `claude_parser`.

### Where tests live

- **Unit tests:** inline `#[cfg(test)] mod tests` in each crate's `src/`. This is the bulk of coverage; `cargo test --workspace` is the gate.
- **Integration:** `crates/<crate>/tests/` — `anthropic_oauth` / `openai_client` use `wiremock`; `balanze_cli/tests/integration_4quadrant.rs` is the end-to-end composition test (real `claude_parser → claude_cost → Snapshot` + `codex_local → Snapshot` against committed fixtures, with a fixed `now` so it can't go wall-clock-flaky).
- **Real-data smokes:** `cargo run -p <crate> --example <name>` against the developer's actual `~/.claude` / `~/.codex` — not run in CI; the maintainer runs them before tagging.
- **`#[ignore]`'d:** the real-keychain smoke (CI keychain is unreliable; run manually per §6).
- **Planned, not yet built:** `predictor` + `watcher` (v0.2), the `src-tauri` Tauri smoke (v0.3), any frontend tests.

What each crate must keep covered is the §6 matrix's "that crate's invariant" rule — the authority is the tests in the tree, not a count here.

## 8. Change Control

**Ask before:**
Schema changes (`UsageEvent`, `Snapshot`, `Settings`, IPC contract), new crate dependencies, invariant changes, cross-crate refactors, touching the actor-model write boundary, adding a new `DegradedState` variant, expanding the secrets surface, adding a new Tauri capability.

**Document:**
Assumptions, trade-offs, and any tech debt introduced. Tech debt that's load-bearing for v0.1 should land as a `// TODO(v0.2):` comment with a one-line explanation of what the eventual fix looks like.

**Tests:**
Add tests when behavior changes or bug fixes could regress. Prefer tests that encode intent and invariants. Do not relax assertions just to make things green.

**Branch & push:**
- `main` is the only protected branch. Never force-push to it.
- Branch naming: `fix/...`, `feat/...`, `docs/...`, `chore/...`. One PR per branch.
- `git push --force-with-lease` is acceptable on feature branches (after a rebase). `git push --force` is not — it can clobber concurrent pushes you didn't see.
- **Never use `--no-verify`** to skip git hooks or `--no-gpg-sign` to skip signing. If a hook fails, fix the underlying issue.
- PRs go through review (human or `code-reviewer` agent) before merge. Squash-merge by default for clean history.

**If you change architecture, update all four:**
`README.md`, `AGENTS.md`, `docs/prd.md`, and the design doc at `~/.gstack/projects/balanze/`. They share the state-coordination diagram, the IPC contract, and the phasing — drift between them is the most likely doc bug. The design doc is the source of truth for architecture; the PRD is the source of truth for product scope; this AGENTS.md is the source of truth for operational and code-discipline rules.

## 9. Communication

- Be concise. Short bullets, concrete next steps.
- Ask targeted questions early when requirements are ambiguous.
- Present 1–3 options with trade-offs when decisions are needed.
- Push back on: security risk, architectural violations, overengineering, violation of best practices, premature scope expansion (this is a side project — alerts and dashboard are v0.3, the UI phase, not v0.1, no matter how easy they look).
- Be correct first, agreeable second.
- Do not add busywork (summary docs, status reports, recap markdown files) unless explicitly asked.
- Persist until the task is complete or genuinely blocked; if blocked, state what you tried and what you need.

## 10a. Known issues

- **Keychain backend broken on Windows (v0.1)**: `keyring = "3.6.3"` (current
  workspace pin) silently no-ops on Windows — `set_password` returns `Ok` but
  the credential never lands in Credential Manager, so subsequent `get_password`
  calls return `NoEntry`. Reproducible via
  `cargo test --release -p keychain -- --ignored`. Workaround: the CLI honors
  `BALANZE_OPENAI_KEY` env var, which takes precedence over the keychain.
  Real fix is migrating to `keyring-core` (the v4 successor crate, which uses
  an explicit "store" initialization pattern rather than the v3 implicit
  default backend). Scheduled for v0.3 alongside the Tauri settings UI, where
  the user will paste their key into a real input box anyway and the
  keychain code will be exercised on both Win and macOS during development.

## 10. Troubleshooting

The v0.1 backend data layer is shipped; the Tauri tray UI is still scaffold. This section captures the footguns the design surfaced plus anything observed in development.

### "Tray icon doesn't appear" or "two tray icons in the menu bar"

The double-tray-icon trap: `tauri.conf.json` declares a default tray with id `"main"`, and code in `lib.rs` creates a second tray via `TrayIconBuilder::new()`. The handler attaches to the invisible second icon; the visible one receives clicks that go nowhere.

Fix: attach the handler via `app.tray_by_id("main").unwrap().on_tray_icon_event(...)`, never via `TrayIconBuilder::new()`. The scaffold already does this correctly in `src-tauri/src/lib.rs`; don't refactor it back.

### "macOS tray click events don't fire"

If the handler is attached correctly (see above) and clicks still don't fire on macOS, check the `iconAsTemplate` setting in `tauri.conf.json`. Template-mode icons can interact strangely with click events on certain macOS versions. The Balanze tray icon should have `iconAsTemplate: false` (the color gauge IS the signal; we don't want macOS inverting it).

### "Predictor returns confidently-wrong numbers right after window reset"

The warm-up state was skipped or the gate was set wrong. Check the `predictor` state machine: for the first 15 minutes after a window reset OR while `events_since_reset < 10`, the predictor MUST return `Insufficient`, not a number. The variance check alone is not enough — right after reset, you have ~0 events and variance is also ~0, which the variance check reads as "high confidence."

### "JSONL parser eats 100% CPU during an active Claude session"

The incremental-read cursor isn't working — parser is doing a full re-parse on every notify event. Check `crates/claude_parser/`: on each watch event the parser should seek to the saved `byte_pos`, read to EOF, parse new lines only, then update the cursor. Full reparse happens only on launch and on explicit `refresh_now()`. Detect atomic rewrites via `(current.size, current.mtime)` vs the stored cursor — never just file size.

### "Two app instances running simultaneously"

`tauri-plugin-single-instance` was either not registered, registered out of order, or its target attribute is wrong. The plugin must be registered **first** on the `tauri::Builder`, and gated `#[cfg(any(target_os = "windows", target_os = "macos"))]`. The scaffold has this wired correctly in `src-tauri/src/lib.rs::run`.

### "Tray icon flickers every 30 seconds"

Tray repaint isn't deduped. The 30s ticker should send `StateMsg::Refresh` to the coordinator; the coordinator should only call `tray.set_icon`/`tray.set_title` if the `(ColorBucket, title_text)` tuple differs from `last_painted`. If you see a flicker during idle periods, the dedup check is missing or comparing wrong fields.

### "`cargo check` fails after bumping a Tauri dep"

`tauri`, `tauri-build`, and `tauri-plugin-*` must all share the same minor version. Mixed minors (e.g. `tauri 2.11` + `tauri-build 2.6`) cause cryptic `generate_context!` macro errors. The workspace `Cargo.toml` pins these together via `workspace.dependencies`; if you bump one, bump them all in lockstep.

### "Frontend can't call my new Tauri command"

The command needs three things wired:
1. Function declared as `#[tauri::command]`.
2. Listed in `tauri::generate_handler![...]` inside `run()`.
3. Capability declared in `src-tauri/capabilities/default.json` (for any non-default API).

Forgetting any of these gives the same opaque error in the frontend. Check `default.json` and the `generate_handler!` block first.

### "Settings file got corrupted after a crash"

The `settings` crate must use the atomic-write pattern: write to `settings.json.tmp`, then `rename` over `settings.json`. Direct writes truncate the existing file before writing new content; a crash mid-write leaves it empty. If you see this, the atomic-write pattern was bypassed.

### "Anthropic Console scrape stopped working overnight"

Expected. Console UI changes will break scrapes regularly — that's why the design defers this to v0.3 and treats it as best-effort. The user-facing fix is to mark the data as stale via `DegradedState::parse_error` and inform the user. Do not try to "make the scrape more robust" by spending a week on it; if the official endpoint isn't there, that's the answer.

### "I want to test against a fixture directory of JSONL"

The committed fixtures are the canonical set: `crates/balanze_cli/tests/fixtures/` (a small Claude-JSONL + Codex-rollout tree the E2E test runs against). For ad-hoc real-data checks, the example smokes read your actual `~/.claude` / `~/.codex`. There is no dedicated parse-root env override in v0.1 — discovery follows `HOME` / `XDG_CONFIG_HOME` (and `CODEX_CONFIG_DIR` for Codex); point those at a fixture tree if you need to redirect it.
