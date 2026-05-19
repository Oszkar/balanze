# Balanze

A local-first utility that consolidates personal AI usage into one normalized
view — Claude subscription quota, an estimate of Claude Code's API-rate value,
OpenAI Codex quota, and real OpenAI API spend, in one glance. Rust + Tauri 2 +
Svelte 5. Side project; Windows 11 and macOS 15+ (CLI also runs on Linux).

> Not affiliated with, endorsed by, or sponsored by Anthropic or OpenAI. Reads
> only endpoints and files you already have access to with your own
> credentials. MIT licensed.

## Status — v0.1 "Data"

v0.1's bar is a **complete, honest data layer** exposed as a CLI
(`balanze-cli`); the tray UI is deliberately a later phase (v0.3). The CLI
prints the same normalized snapshot the eventual popover will show. The
four-quadrant matrix is fully lit:

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
  issues).
- **OpenAI Codex quota** — reads the local Codex CLI rollout files
  (`~/.codex/sessions/`) for the server-computed `rate_limits.primary` %.
- **OpenAI API $** — `/v1/organization/costs` with an `sk-admin-…` key:
  this-month spend + per-line-item breakdown. Real billing data.

Already on `main` past the v0.1.0 tag: proactive OAuth token refresh (the
bearer no longer hard-fails every ~8 h) and a server-anchored cap window
(**v0.1.1 base**), plus a shared source-orchestration policy and an HTTP
backoff layer (**v0.2 de-risk** — no user-facing behavior change).

Roadmap themes: **Data → Liveness → UI → Distribution**. Full history in
[`CHANGELOG.md`](CHANGELOG.md); phase detail in [`docs/prd.md`](docs/prd.md);
architecture and code discipline in [`AGENTS.md`](AGENTS.md).

## CLI

`balanze-cli` is the v0.1 surface and the reference composition for the
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
balanze-cli set-openai-key [KEY]  Store an sk-admin-… key in the OS keychain
balanze-cli clear-openai-key      Remove the OpenAI key from the keychain
balanze-cli settings              Print current settings.json
balanze-cli help                  This help

Env override: BALANZE_OPENAI_KEY=sk-admin-…  (takes precedence over the
keychain; recommended on Windows until the keyring-v4 migration — see
Known issues).
```

Default compact view — the four quadrants on one screen, with a legend that
keeps the *estimated* Anthropic cell from being mistaken for the *real*
OpenAI bill:

```text
=== Balanze status (2026-05-15 04:27:42 UTC) ===

                    Quota %                                 API $
Anthropic           ✓ 82.0% 5h, 88.0% 7d (oauth)            ~$2197.11 (est. list-price, not billed)
OpenAI              ✓ 6.0% 7d (codex go)                    ○ not configured (run `balanze-cli setup`)

Quota % = live server-reported utilization. API $: Anthropic =
estimated list-price for local Claude Code tokens (subscription
leverage — NOT money you were billed); OpenAI = real billed spend.
```

`--sections` expands each source (cadence bars + reset clocks, the per-model
JSONL breakdown + burn rate, the estimated-cost detail with LiteLLM
provenance, the Codex window, OpenAI spend by line item). `--json` emits the
full `Snapshot` for scripting.

## Install

v0.1 ships **from source only** — no binaries, installers, or GitHub
Releases, and it is **not on crates.io** (signed binaries / Homebrew /
WinGet are the v0.4 Distribution phase). Requires Rust 1.77+.

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

# Desktop app (scaffold only — tray icon, no data yet; the real UI is v0.3):
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

This is **not needed for v0.1** — if you only want the CLI on Linux, never
run a `--workspace` build and you'll never see a `gdk-3.0`/`pango`/`cairo`
error. Test discipline and the per-crate validation matrix live in
`AGENTS.md` §6–§7.

## Known issues

- **Keychain backend broken on Windows.** `keyring 3.6.3` silently no-ops:
  `set_password` returns `Ok` but the credential never lands in Credential
  Manager. Workaround: set `BALANZE_OPENAI_KEY`. Fix scheduled for v0.3
  (`keyring` → `keyring-core` v4, riding with the settings UI that exercises
  the key-input box on both platforms). Detail: `AGENTS.md` §10a.

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
