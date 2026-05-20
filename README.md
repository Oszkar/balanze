# Balanze

[![CI](https://img.shields.io/github/actions/workflow/status/Oszkar/balanze/ci.yml?branch=main&label=ci&logo=github)](https://github.com/Oszkar/balanze/actions/workflows/ci.yml)
[![Version](https://img.shields.io/github/v/tag/Oszkar/balanze?label=version&color=blue)](https://github.com/Oszkar/balanze/tags)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.77%2B-orange?logo=rust&logoColor=white)](Cargo.toml)

A local-first utility that consolidates personal AI usage into one normalized
view — Claude subscription quota, an estimate of Claude Code's API-rate value,
OpenAI Codex quota, and real OpenAI API spend, in one glance. Rust + Tauri 2 +
Svelte 5. Side project; Windows 11 and macOS 15+ (CLI also runs on Linux).

> Not affiliated with, endorsed by, or sponsored by Anthropic or OpenAI. Reads
> only endpoints and files you already have access to with your own
> credentials. MIT licensed.

## What it does

A **complete, honest data layer** exposed as a CLI (`balanze-cli`); a tray UI
is on the roadmap. The CLI prints the same normalized snapshot the eventual
popover will show. The four-quadrant matrix:

|               | Quota %                              | API $                                            |
|---------------|--------------------------------------|--------------------------------------------------|
| **Anthropic** | OAuth usage (5h / 7-day / per-model)  | estimated list-price from JSONL (**not** billed) |
| **OpenAI**    | Codex CLI rate-limit %                | real billed spend (Admin Costs API)              |

- **Anthropic quota** — the same `/api/oauth/usage` endpoint Claude Code uses:
  live 5-hour / 7-day / per-model bars + `resets_at` clocks. No scraping.
- **Anthropic API $ (estimated)** — `claude_cost` synthesizes a list-price
  equivalent from local JSONL × a vendored LiteLLM price table. For Pro/Max
  users this is **subscription leverage, not money billed** — the CLI labels
  it that way (Anthropic's real cost API is enterprise-gated; see Known
  issues). If you enabled "Extra usage" pay-as-you-go on claude.ai, the
  OAuth feed's `extra_usage` block (real billed cents — the same number
  claude.ai's overage meter shows) is surfaced on a separate line so it
  can't be confused with the estimate.
- **OpenAI Codex quota** — reads the local Codex CLI rollout files
  (`~/.codex/sessions/`) for the server-computed `rate_limits.primary` %.
- **OpenAI API $** — `/v1/organization/costs` with an `sk-admin-…` key:
  this-month spend + per-line-item breakdown. Real billing data.

Planning and history live elsewhere: roadmap and phase detail in
[`docs/prd.md`](docs/prd.md); release history in
[`CHANGELOG.md`](CHANGELOG.md); architecture and code discipline in
[`AGENTS.md`](AGENTS.md).

## CLI

`balanze-cli` is the current surface and the reference composition for the
eventual tray popover.

```text
balanze-cli                       4-quadrant compact status (default)
balanze-cli status [--json] [--sections] [-v]
                                  --sections  per-source detail (cadence bars,
                                              model breakdown, codex window)
                                  --json      machine-readable Snapshot JSON
                                              (wins over --sections if both)
                                  -v          account-identifying fields
                                              (org uuid, codex session_id)
balanze-cli setup                 Interactive wizard — run this first
balanze-cli set-openai-key        Store an sk-admin-… key in the OS keychain
                                  (masked TTY prompt, or piped stdin)
balanze-cli clear-openai-key      Remove the OpenAI key from the keychain
balanze-cli settings              Print current settings.json
balanze-cli statusline            Claude Code statusLine command: reads the
                                  statusLine JSON on stdin, prints a one-line
                                  status (live 5h/7d quota + session cost).
balanze-cli help                  This help

Env override: BALANZE_OPENAI_KEY=sk-admin-…  (takes precedence over the
keychain; recommended on Windows until the keyring-v4 migration — see
Known issues).
```

`balanze-cli statusline` is Claude Code's statusLine command (offered by `balanze-cli setup`) — shows live 5h/7d subscription quota + session cost in your shell; zero-auth, no rate limit.

Default compact view — the four quadrants on one screen, with a legend that
keeps the *estimated* Anthropic cell from being mistaken for the *real*
OpenAI bill:

```text
=== Balanze status (2026-05-20 04:27:42 UTC) ===

                    Quota %                                 API $
Anthropic           ✓ 82.0% 5h, 88.0% 7d (oauth)            $20.92/$25.00 overage billed · ~$2197.11 est-leverage (not billed)
OpenAI              ✓ 6.0% 7d (codex go)                    ✓ $4.20 this month (real)

Quota % = live server-reported utilization. API $: Anthropic =
estimated list-price for local Claude Code tokens (subscription
leverage — NOT billed). 'overage billed' = REAL pay-as-you-go
spend from Anthropic. OpenAI = real billed spend.
```

`--sections` expands each source (cadence bars + reset clocks, the per-model
JSONL breakdown + burn rate, the estimated-cost detail with LiteLLM
provenance, the Codex window, OpenAI spend by line item). `--json` emits a
machine-readable document where every money cell is `{ value_micro_usd,
source, confidence, details }`, so a script can read
`.anthropic_api_cost.value_micro_usd` and `.openai.value_micro_usd` uniformly
and tell `jsonl_list_price`/`estimate` apart from `openai_admin_costs`/`real`
without parsing labels. `--json -v` adds account identifiers
(`org_uuid`, Codex `session_id`); without `-v` they're redacted.

## Install

Balanze currently ships **from source only** — no binaries, installers, or
GitHub Releases, and it is **not on crates.io** (signed binaries, Homebrew,
and WinGet are on the roadmap; see [`docs/prd.md`](docs/prd.md)). Requires
Rust 1.77+.

```bash
# `--git` is required (not on crates.io). The repo root is a virtual
# workspace, so name the package explicitly — it builds the `balanze-cli`
# binary. Plain `cargo install balanze_cli` will NOT work.
cargo install --git https://github.com/Oszkar/balanze balanze_cli
balanze-cli setup      # run this first — wizard for the OpenAI admin key
balanze-cli            # 4-quadrant status
```

**The CLI has zero system-library dependencies** — Windows 11, macOS 15+,
and Linux build with just the Rust toolchain (Linux also needs a C compiler
for the `ring` TLS dependency). No GTK/GLib/Cairo/WebKit — that native stack
belongs to the desktop app, not the CLI.

The Claude side reads `~/.claude/.credentials.json` directly — no setup
needed if Claude Code is already configured. Provide the OpenAI Admin key
via `balanze-cli setup`, `set-openai-key`, or the `BALANZE_OPENAI_KEY` env
var (recommended on Windows; see Known issues).

## Develop

Prerequisites: Rust 1.77+ (all you need for the CLI); Bun 1.3+ (only for the
Svelte frontend scaffold / `tauri dev`).

```bash
# CLI from the workspace:
cargo run --release -p balanze_cli -- status

# Full workspace checks:
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
bun run check                                  # svelte-check + tsc

# Desktop app (scaffold only — tray icon, no data yet; real UI is on the roadmap):
bun install                                    # also installs git hooks (see below)
bun run tauri dev
```

`bun install` runs `lefthook install` (skipped without `.git/`), wiring
`commit-msg` (Conventional Commits — blocking), `pre-commit` (rustfmt +
svelte-check) and `pre-push` (clippy + tests) so the gates CI enforces fail
locally first. Bypass one commit with `git commit --no-verify`, or
`LEFTHOOK=0` for a session.

**`default-members = ["crates/*"]`:** bare `cargo build`/`test`/`run` skip
`src-tauri`, so a CLI build never needs GUI libraries. The desktop app is
the explicit opt-in (`cargo build --workspace` or `bun run tauri dev`) and
pulls in the platform GUI stack:

- **Windows:** WebView2 runtime + VS Build Tools (no GTK — Tauri uses WebView2).
- **macOS:** Xcode Command Line Tools.
- **Debian/Ubuntu:** `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev build-essential libssl-dev libglib2.0-dev pkg-config`

**Not needed for the CLI** — if you only want the CLI on Linux, never
run a `--workspace` build and you'll never see a `gdk-3.0`/`pango`/`cairo`
error. Test discipline and the per-crate validation matrix live in
`AGENTS.md` §6–§7.

## Known issues

- **Keychain backend broken on Windows.** `keyring 3.6.3` silently no-ops:
  `set_password` returns `Ok` but the credential never lands in Credential
  Manager. Workaround: set `BALANZE_OPENAI_KEY`. Fix is scheduled to ride
  with the settings UI (`keyring` → `keyring-core` v4 migration). Detail:
  `AGENTS.md` §10a.

## Layout

```
balanze/
├── Cargo.toml                workspace root (Rust 2021, MSRV 1.77)
├── package.json              bun + Svelte 5 + TypeScript + Vite
├── src/                      Svelte frontend (scaffold today)
├── src-tauri/                Tauri 2 app crate (scaffold tray + single-instance)
├── crates/
│   ├── claude_parser/        JSONL parser + walker + dedup + IncrementalParser
│   ├── claude_cost/          pure JSONL→estimated-$ synth (vendored LiteLLM prices)
│   ├── claude_statusline/    Claude Code statusLine payload parser + settings.json wiring
│   ├── anthropic_oauth/      /api/oauth/usage client + credentials read/write
│   ├── openai_client/        OpenAI /v1/organization/costs client
│   ├── codex_local/          reads ~/.codex/sessions/ for Codex rate-limit %
│   ├── window/               pure rolling-window math (5h + 30m burn rate)
│   ├── backoff/              exponential-backoff policy + generic async retry
│   ├── snapshot_composer/    single source-orchestration policy (CLI ≡ watcher)
│   ├── state_coordinator/    actor crate; owns Snapshot, notifies Sink
│   ├── settings/             non-secret settings.json (atomic write)
│   ├── keychain/             OS keychain wrapper (only consumer of `keyring`)
│   └── balanze_cli/          CLI entry-point composing the backend crates
├── docs/prd.md               product spec + phasing
├── AGENTS.md                 operational contract for AI agents / contributors
└── .github/workflows/        CI (Linux always; Win+Mac scheduled) + release matrix
```

## Contributing

Not actively soliciting contributions yet — this is a personal tool first.
Found a bug or want to discuss design? Open an issue. Sending a PR anyway?
Read `AGENTS.md` first; it codifies the architectural boundaries and
code-discipline rules.

## License

MIT — see `LICENSE`.
