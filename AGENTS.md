# AGENTS.md — Operational Contract

Repo: `Oszkar/balanze` | Branch: `main` | Agents: Claude Code, Copilot, Gemini, Windsurf, Codex

This file is the source of truth for code-discipline rules. Architecture (state diagram, crate map, the twelve boundaries, IPC contract) lives in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). Product scope and phasing live in [`docs/PRD.md`](docs/PRD.md).

## 0. Prime Rule: Clarify Before Acting

If requirements are ambiguous, incomplete, or conflicting: stop, ask targeted questions, propose 1–3 concrete interpretations, and either wait for confirmation OR proceed with the assumption stated explicitly, depending on impact.

**Calibration:**

- High-impact / hard-to-reverse (changes to `UsageEvent` / `Snapshot` schema, the IPC contract, the actor-model write boundary, the keychain wrapper, anything touching secrets, `settings.json` schema, new Tauri capabilities) → **wait**.
- Low-impact / reversible (a clippy fix, a unit test, a CSS tweak, a doc reword, a new tray menu item, a log-level adjustment, a non-breaking refactor inside a single module) → **state the assumption and proceed**.
- When in doubt, wait.

## 1. System Context

Balanze = a local-first desktop tray utility that consolidates personal AI usage tracking into one normalized view. Tauri 2 + Rust + Svelte 5. Currently targeting Windows 11 and macOS 15+; later phases might add Ubuntu GNOME, Android, and a hosted wallboard.

Out of scope: full enterprise cost allocation, multi-seat observability, browser automation, monetization, cloud sync.

## 2. Engineering Principles

Apply at all times:

- **12-Factor App** — Config in env, stateless where possible, strict dev/prod parity.
- **DRY** — No duplication of domain logic. JSONL parsing happens in one crate; rolling-window math in one crate; etc.
- **YAGNI** — No speculative abstractions. The crate set is fixed (see `docs/ARCHITECTURE.md`); don't add a new crate because it "might be useful."
- **KISS** — Simplest viable implementation.
- **PoLP** — Least privilege always.
- **MVP Bias** — Ship fast, document tech debt, don't gold-plate, don't architect for imaginary scale.

Correctness > Cleverness · Security > Convenience · Simplicity > Flexibility · Precision > Agreeability

### 2.1 Project conventions

| Concern | Convention |
|---|---|
| Rust edition | 2024 (workspace). Migrated 2026-05-29. The former "Tauri 2 macros lag on edition 2024" pin was stale: it conflated Tauri **1.x** (still catching up — [tauri#10412](https://github.com/tauri-apps/tauri/issues/10412), backported only in [tauri#15207](https://github.com/tauri-apps/tauri/pull/15207), Apr 2026) with **2.x**, which fixed edition-2024 `Cargo.toml` parsing (`tauri-build`/`tauri-codegen` via `cargo_toml` ≥ 0.20) before the edition stabilized. Balanze is on Tauri 2 |
| Rust MSRV | 1.85 (workspace `Cargo.toml`; the floor edition 2024 requires). Toolchain pinned to 1.94.0 via `rust-toolchain.toml` - the single pin source: CI installs it with `actions-rust-lang/setup-rust-toolchain@v1`, which reads that file, so a toolchain bump is one file + this row |
| Workspace | Single Cargo workspace at repo root; `src-tauri` + `crates/*` are members; shared deps declared at workspace level |
| Logging | `tracing` (not `log`); see §3.2 for level discipline |
| Async | `tokio` everywhere; never block the runtime; never hold a `tokio::sync::Mutex` across an unrelated `.await` |
| Errors | `anyhow::Result<T>` at app boundaries; `thiserror` enums ONLY inside `claude_parser` (the StateCoordinator pattern-matches `FileMissing` / `SchemaDrift`) |
| Currency | `i64` micro-USD (1e-6 USD) internally; convert to `f64` only at the display boundary. Never use `f64` for sums or threshold comparisons |
| Cap unit | Tokens for the Claude subscription rolling-window cap; micro-USD for the OpenAI API cap. No synthetic-dollar pricing on the cap math path |
| Frontend | Svelte **5 runes** (`$state`, `$derived`, `$props`). SvelteKit with `adapter-static` in SPA mode. Vite 8. TypeScript strict. Env via `import.meta.env.VITE_*` |
| IPC contract | Frontend ↔ backend: only via the commands + events in `docs/ARCHITECTURE.md`. Adding to this surface needs a doc update first |
| CLI `--json` schema | Presentation DTO (`crates/balanze_cli/src/json_output.rs`) — see `docs/ARCHITECTURE.md` "IPC contract". Schema changes require updating that module's tests + `README.md` + `docs/ARCHITECTURE.md` |
| Watcher cadence | `Settings::oauth_poll_interval_secs` (default 300; serde-default 300 on absent key). Each poller clamps to `max(300, value)` to honor §3.1 regardless of `settings.json` |
| Code style | `cargo fmt` defaults own Rust. Markdown has **no** column cap — never reflow a doc to hit a width. `prettier` not configured |
| Lint floor | `cargo clippy --workspace --all-targets -- -D warnings` passes; `bun run check` passes |
| Commits | Conventional Commits, enforced by a blocking `commit-msg` lefthook hook. `<type>(scope)?(!)?: subject`. Squash-merge lands the PR title on `main`, so PR titles must match - also CI-validated by `.github/workflows/pr-title.yml` |

## 3. Non-Negotiables

### 3.1 API politeness toward providers

There is no internal rate-limit gate — the only thing being rate-limited is *us* against Anthropic and OpenAI. Rules:

- OpenAI billing endpoints: poll at most every 5 minutes.
- Anthropic Console cookie-paste (demoted 2026-05-19 to opt-in): if/when it lands, poll at most every 5 minutes; back off on 429 with exponential backoff (start 30s, cap 10 min).
- Claude JSONL: local file I/O — read **incrementally** via per-file byte cursor (`HashMap<PathBuf, FileCursor { byte_pos, mtime, size }>`). Full reparse only on launch or `refresh_now()`. Detect atomic rewrites (size unchanged but mtime changed) and truncations (size < cursor).
- Tray icon repaint: 30s cadence, **deduped** by `(ColorBucket, title_text)`. Never call `tray.set_icon` if the bucket and title haven't changed.
- HTTP retry/backoff: implemented in `anthropic_oauth` + `openai_client` via the `backoff` crate. Idempotent GETs retry on 429 + 5xx + transport; the token-rotating `refresh_access_token` POST retries 429-only. CLI passes `BackoffPolicy::fail_fast()` (0 retries); the watcher passes `BackoffPolicy::standard()` (30s start, 10-min cap).

### 3.2 Error handling & logging

**Errors:** `anyhow::Result<T>` at app boundaries; no `.unwrap()` outside tests. `thiserror` enums live only in `claude_parser`. Tauri commands return `Result<T, String>` derived from `anyhow::Error::to_string()`. Long-running tasks must be supervised (retained `JoinHandle` + `tokio::select!`). External I/O uses exponential backoff via the `backoff` crate; never tight-loop on failure. IPC-boundary errors surface to the UI as the `degraded_state` event.

**Logging (`tracing` crate):**

| Level | Use for |
|---|---|
| `error` | Operator must look — supervisor exits, persistent keychain failures, repeated parse errors after schema drift |
| `warn` | Recoverable but worth noticing — OpenAI 429 retry, watcher restart, atomic-rewrite cursor invalidation, dropped state-coordinator mpsc message |
| `info` | Normal lifecycle — app start, first JSONL parse complete, OpenAI tile populated, settings saved, window-reset transition observed |
| `debug` | Per-event detail — individual JSONL line parsed, state-coordinator message handled, window pace recomputed |
| `trace` | Raw frame dumps; almost never enabled |

Default level: `INFO` for app modules, `WARN` for the parser (DEBUG-per-file JSONL parsing is gated behind `BALANZE_LOG=debug,balanze::claude_parser=trace`). Logs rotate via `tracing-appender` (5 MB max, keep last 3). Don't log secrets at any level. Periodic logs cap at one line per N minutes; never one-per-event at info level.

### 3.3 Legal context

Balanze reads (1) the user's own local Claude JSONL at `~/.claude/projects/**/*.jsonl`, (2) OpenAI's documented billing API (`/v1/usage`, `/v1/dashboard/billing/*`), and (3) optionally the Anthropic Console cookie-paste (opt-in only — if and when it lands, scraped data is `DataSource::AnthropicConsoleScrape` with `Confidence::Estimated` and the user is informed it may break).

Personal use only. Not affiliated with Anthropic or OpenAI. If a provider revokes access or breaks a scrape, degrade gracefully (mark data stale).

### 3.4 Secret hygiene

Secrets in scope: user-supplied OpenAI API keys, plus read access to Claude Code's OAuth tokens at `~/.claude/.credentials.json` (and `~/.config/claude/.credentials.json` on newer Claude Code installs).

- **Storage.** OpenAI keys live in the OS keychain via the `keyring` crate. The Claude OAuth file is Anthropic's; we reuse it. Neither is ever written to disk in plaintext outside those locations. `.env` is gitignored and not loaded — non-secret config goes through `directories::ProjectDirs`, env-var overrides are documented in CLI help (`BALANZE_OPENAI_KEY`, `BALANZE_LOG`).
- **Logging.** Never log any secret at any level. The only acceptable display surface outside the settings UI's masked input is a redacted form (`sk-…45 (len=51)`) in a hypothetical debug "show config" command. The settings UI's API-key input is `type="password"`. File paths (and their existence) are loggable at INFO; their contents are not.
- **Single writer.** `anthropic_oauth` is the only crate that reads or writes `~/.claude/.credentials.json`. The only write is the refreshed-token write-back: atomic tmp + fsync + rename, perms-preserving, reuses Anthropic's file, touches only `claudeAiOauth` token fields, never regresses a concurrently-newer on-disk token. We do not mirror, persist, or back up this file's contents anywhere.
- **Pre-commit scan.** A lefthook pre-commit hook (`scripts/check-secrets.mjs`) scans staged content for key-shaped strings (OpenAI `sk-...` keys, `BALANZE_OPENAI_KEY` literals, high-entropy assignments) and blocks `.env` files outright.
- **Surface discipline.** New secrets require a clear rotation path and a `DegradedState` variant for "credential unavailable / expired" before they're added. If a user-supplied key leaks, the user rotates at the provider. If a Claude OAuth token leaks, the user re-runs `claude login`. Balanze stores no audit trail of historical credentials.

### 3.5. Misc.

- DO NOT use em-dashes (—), use regular hyphens (-) instead including in code, PR descriptions, everywhere.
- DO NOT use the Unicode ellipsis character at the end of sentences, use three periods (...) instead.
- Avoid exposing project management jargon, task IDs, etc. into commit messages, PR titles, and publicly facing content (UI, changelog, etc.)
- Soft wrapping is highly desired in Markdown. Don't constrain text to a certain number of characters in a line.

## 4. Architecture

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — data-flow diagram, crate map, the twelve numbered boundaries, IPC contract, error/degraded-state discipline.

When the architecture changes, update both files in lockstep along with `README.md` and `docs/PRD.md`. They share the boundary list and the IPC contract; drift between them is the most common doc bug.

## 5. Quick Start for Agents

### Every task

1. Identify the correct crate / layer (see `docs/ARCHITECTURE.md`).
2. List invariants affected by your change (which boundary, which conventions in §2.1).
3. Implement the smallest safe diff that fixes the root cause.
4. Validate (see §6).
5. If ambiguous → ask targeted questions early.
6. Use context7 MCP for looking up Tauri / Svelte / crate docs and verifying external library signatures (Tauri 2's v1→v2 migration left a lot of stale docs in the wild).

Avoid drive-by refactors.

### Local dev (for agents that can run commands)

Preferred loop (`just` recipes wrap the raw commands below):

```bash
just check                       # rustfmt + clippy -D warnings + svelte-check + cargo deny
just test                        # cargo-nextest + vitest
just dev                         # run the desktop app
```

Raw commands (what the recipes run, plus extras):

```bash
# Run the desktop app:
bun run tauri dev

# Subsystem checks (fast feedback):
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace    # or: cargo test --workspace
bun run check                    # svelte-check + tsc

# Release build:
bun run tauri build              # produces MSI/NSIS on Win, DMG/app on Mac
```

Hot-reload: `bun run tauri dev` hot-reloads the Svelte frontend on save. The Rust backend does **not** — restart manually. For tight Rust iteration use `cargo watch -x 'check --workspace'` in a side terminal.

## 6. Validation Matrix

Before claiming work is done:

| Change touches | Required gates |
|---|---|
| Any `**/*.rs` in workspace | `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo nextest run --workspace`, `cargo fmt --all -- --check` |
| Any crate's logic | That crate's own tests stay green **and** you add/extend a test for the specific invariant you changed. For pure crates (`window`, `claude_cost`): write the test **before** the impl change. Don't weaken an assertion to make it pass — see §7 |
| `crates/claude_parser/**` | + documented failure modes still pass (file missing / partial final line / schema drift / empty / permission denied) + real-data smoke (`cargo run -p claude_parser --example claude_parser_smoke`) |
| `crates/keychain/**` | + run the `#[ignore]`'d real-keychain smoke **manually on each OS before tagging a release** — cross-OS keychain in CI is unreliable, so CI green ≠ keychain works |
| `crates/{anthropic_oauth,openai_client}/**` | + the wiremock suite; manual check against the real endpoint on the dev machine |
| `src-tauri/src/**/*.rs` | + `bun run tauri dev` smoke (tray icon appears, click opens window, Quit exits cleanly) |
| `src-tauri/tauri.conf.json` | + `bun run tauri build --no-bundle` succeeds (validates config without the slow bundle) |
| `src/**/*.{svelte,ts}` | `bun run check`; visually verify in `bun run tauri dev` |
| Workspace `Cargo.toml` | `cargo check --workspace`; `cargo tree --workspace --duplicates` if you added a dep |
| `.github/workflows/*.yml` | YAML lints clean; for `release.yml`, trigger via `workflow_dispatch` after merge before tagging |

`cargo clippy -D warnings` is **strict**. CI enforces it. Don't add `#[allow]` to silence lints unless there's a documented technical reason in a comment immediately above.

**If unable to run:** state the exact command and request output from a human. Don't claim the work is done.

## 7. Test Discipline

- Run the smallest meaningful set first; expand based on risk.
- Don't weaken assertions or modify tests without invariant reasoning.
- If you find unrelated failures, call them out separately with evidence.
- Tests encode invariants — treat them accordingly.
- **When you change a load-bearing pure function, add a test before changing the implementation.**

### Where tests live

- **Unit tests:** inline `#[cfg(test)] mod tests` in each crate's `src/`. `cargo nextest run --workspace` is the gate (plain `cargo test --workspace` works too).
- **Integration:** `crates/<crate>/tests/` — `anthropic_oauth` / `openai_client` use `wiremock`; `balanze_cli/tests/integration_4quadrant.rs` is the end-to-end composition test against committed fixtures with a fixed `now`.
- **Real-data smokes:** `cargo run -p <crate> --example <name>` against the developer's actual `~/.claude` / `~/.codex` — not run in CI; maintainer runs them before tagging.
- **`#[ignore]`'d:** the real-keychain smoke (CI keychain is unreliable; run manually per §6).

## 8. Change Control

**Ask before:** schema changes (`UsageEvent`, `Snapshot`, `Settings`, IPC contract), new crate dependencies, invariant changes, cross-crate refactors, touching the actor-model write boundary, adding a new `DegradedState` variant, expanding the secrets surface, adding a new Tauri capability.

**Document:** assumptions, trade-offs, tech debt. Load-bearing tech debt lands as a plain `// TODO:` with a one-line note on the eventual fix. Keep code comments free of ephemeral project/release nomenclature (release/version tags like `v0.2`, track/phase/milestone labels, spike references) — that framing belongs in the PRD and the changelog, not in the code; durable spec cross-references (e.g. `AGENTS.md §3.1`) are fine.

**Tests:** add tests when behavior changes or fixes could regress. Tests encode intent and invariants. Don't relax assertions to make things green.

**Branch & push:**

- `main` is the only protected branch. Never force-push to it.
- Protection is enforced by a repository ruleset on `main`: changes land via PR only, with required status checks (`linux`, `cargo-deny`, `conventional-commit title`); branch deletion and non-fast-forward pushes are blocked.
- Branch naming: `fix/...`, `feat/...`, `docs/...`, `chore/...`. One PR per branch.
- `git push --force-with-lease` is acceptable on feature branches; `git push --force` is not.
- **Never use `--no-verify`** to skip git hooks or `--no-gpg-sign` to skip signing. If a hook fails, fix the issue.
- PRs go through review before merge. Squash-merge by default.

**If you change architecture, update all docs as needed:** `README.md`, `AGENTS.md`, `docs/ARCHITECTURE.md`, `docs/PRD.md`.

## 9. Communication

- Be concise. Short bullets, concrete next steps.
- Ask targeted questions early when requirements are ambiguous.
- Present 1–3 options with trade-offs when decisions are needed.
- Push back on: security risk, architectural violations, overengineering, premature scope expansion.
- Be correct first, agreeable second.
- Don't add busywork (summary docs, status reports, recap Markdown) unless explicitly asked.
- Persist until the task is complete or genuinely blocked; if blocked, state what you tried and what you need.

## 10a. Known issues

- **Keychain backend broken on Windows (v0.1):** `keyring = "3.6.3"` silently no-ops on Windows — `set_password` returns `Ok` but the credential never lands in Credential Manager. Reproducible via `cargo test --release -p keychain -- --ignored`. Workaround: the CLI honors `BALANZE_OPENAI_KEY`, which takes precedence over the keychain. Real fix is migrating to `keyring-core` (v4).

## 10. Troubleshooting

### "Tray icon doesn't appear" or "two tray icons in the menu bar"

The double-tray-icon trap: `tauri.conf.json` declares a default tray with id `"main"`, and code in `lib.rs` creates a second tray via `TrayIconBuilder::new()`. The handler attaches to the invisible second icon; the visible one receives clicks that go nowhere.

Fix: attach the handler via `app.tray_by_id("main").unwrap().on_tray_icon_event(...)`, never via `TrayIconBuilder::new()`. The scaffold already does this correctly in `src-tauri/src/lib.rs`; don't refactor it back.

### "macOS tray click events don't fire"

If the handler is attached correctly (above) and clicks still don't fire on macOS, check `iconAsTemplate` in `tauri.conf.json`. Template-mode icons can interact strangely with click events on certain macOS versions. Balanze's tray icon should have `iconAsTemplate: false` (the color gauge IS the signal; we don't want macOS inverting it).

### "JSONL parser eats 100% CPU during an active Claude session"

The incremental-read cursor isn't working — the parser is doing a full re-parse on every notify event. Check `crates/claude_parser/`: on each watch event the parser should seek to the saved `byte_pos`, read to EOF, parse new lines only, then update the cursor. Full reparse happens only on launch and on explicit `refresh_now()`. Detect atomic rewrites via `(current.size, current.mtime)` vs the stored cursor — never just file size.

### "Two app instances running simultaneously"

`tauri-plugin-single-instance` was either not registered, registered out of order, or its target attribute is wrong. The plugin must be registered **first** on the `tauri::Builder`, gated `#[cfg(any(target_os = "windows", target_os = "macos"))]`. The scaffold wires this correctly in `src-tauri/src/lib.rs::run`.

### "Tray icon flickers every 30 seconds"

Tray repaint isn't deduped. The 30s ticker should send `StateMsg::Refresh` to the coordinator; the coordinator should only call `tray.set_icon`/`tray.set_title` if the `(ColorBucket, title_text)` tuple differs from `last_painted`. If you see flicker during idle, the dedup check is missing or comparing the wrong fields.

### "`cargo check` fails after bumping a Tauri dep"

`tauri`, `tauri-build`, and `tauri-plugin-*` must all share the same minor version. Mixed minors (e.g. `tauri 2.11` + `tauri-build 2.6`) cause cryptic `generate_context!` macro errors. The workspace `Cargo.toml` pins these together via `workspace.dependencies`; if you bump one, bump them all in lockstep.

### "Frontend can't call my new Tauri command"

The command needs three things wired: (1) function declared `#[tauri::command]`, (2) listed in `tauri::generate_handler![...]` inside `run()`, (3) capability declared in `src-tauri/capabilities/default.json` (for any non-default API). Forgetting any of these gives the same opaque error. Check `default.json` and the `generate_handler!` block first.

### "Settings file got corrupted after a crash"

The `settings` crate must use the atomic-write pattern: write to `settings.json.tmp`, then `rename` over `settings.json`. Direct writes truncate the existing file before writing new content; a crash mid-write leaves it empty. If you see this, the atomic-write pattern was bypassed.

### "Anthropic Console scrape stopped working overnight"

Expected. Console UI changes will break scrapes regularly — that's why the design defers this to v0.3 (now opt-in) and treats it as best-effort. Mark the data stale via `DegradedState::parse_error` and inform the user. Don't try to "make the scrape more robust" by spending a week on it; if the official endpoint isn't there, that's the answer.