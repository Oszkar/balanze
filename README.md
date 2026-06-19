# Balanze

[![CI](https://img.shields.io/github/actions/workflow/status/Oszkar/balanze/ci.yml?branch=main&label=ci&logo=github)](https://github.com/Oszkar/balanze/actions/workflows/ci.yml)
[![Version](https://img.shields.io/github/v/tag/Oszkar/balanze?label=version&color=blue)](https://github.com/Oszkar/balanze/tags)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?logo=rust&logoColor=white)](Cargo.toml)

A local-first utility that consolidates personal AI usage into one normalized view — Claude subscription quota, an estimate of Claude Code's API-rate value, OpenAI Codex quota, and real OpenAI API spend, in one glance. Rust + Tauri 2 + Svelte 5. Side project; Windows 11 and macOS 15+ (CLI also runs on Linux).

> Not affiliated with, endorsed by, or sponsored by Anthropic or OpenAI. Reads only endpoints and files you already have access to with your own credentials.

## What it does

A **complete, honest data layer** surfaced two ways — the `balanze-cli` CLI and a **tray popover** (v0.3.0: a color-shifting gauge tray icon + a glanceable grid/cards view). Both render the same normalized snapshot. The matrix holds **measured reality only** — server-reported quota % and real billed $ — so cells in a column are always the same *kind* of number:

|               | Quota %                              | API $ (real billed)                                 |
|---------------|--------------------------------------|-----------------------------------------------------|
| **Anthropic** | OAuth usage (5h / 7-day / per-model) | `extra_usage` overage if you enabled it, else *n/a* |
| **OpenAI**    | Codex CLI rate-limit %               | real billed spend (Admin Costs API)                 |

The JSONL list-price estimate is **not** a matrix cell — it's a separate **"Subscription leverage"** insight (see below).

- **Anthropic quota** — the same `/api/oauth/usage` endpoint Claude Code uses: live 5-hour / 7-day / per-model bars + `resets_at` clocks. No scraping.
- **Anthropic API $ — real or nothing.** Anthropic exposes no per-user API spend, so this cell shows the real `extra_usage` pay-as-you-go overage *if* you enabled it on claude.ai (the same billed cents claude.ai's overage meter shows), and otherwise reads as **not available** — never backfilled with a substitute number.
- **Subscription leverage (estimate, separate)** — `claude_cost` synthesizes a list-price equivalent from local JSONL × a vendored LiteLLM price table: what your Claude Code usage *would* cost at API list prices. For Pro/Max users this is **leverage from the subscription, never billed** — so it's presented as its own insight, outside the matrix, where it can't be mistaken for spend.
- **OpenAI Codex quota** — reads the local Codex CLI rollout files (`~/.codex/sessions/`) for the server-computed `rate_limits.primary` %.
- **OpenAI API $** — `/v1/organization/costs` with an `sk-admin-…` key: this-month spend + per-line-item breakdown. Real billing data.

Planning and history live elsewhere: roadmap and phase detail in [`docs/PRD.md`](docs/PRD.md); architecture and boundaries in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md); release history in [`CHANGELOG.md`](CHANGELOG.md); code discipline in [`AGENTS.md`](AGENTS.md).

## CLI

`balanze-cli` is the headless surface and the reference composition the tray popover (v0.3.0) renders.

```text
balanze-cli                       4-quadrant compact status (default)
balanze-cli status [--json] [--sections] [-v]
                                  --sections  per-source detail (cadence bars,
                                              model breakdown, codex window)
                                  --json      machine-readable Snapshot JSON
                                              (wins over --sections if both)
                                  -v          account-identifying fields
                                              (org uuid, codex session_id)
balanze-cli --watch [--json]      Long-running refresh loop. Without --json,
                                  redraws the compact status in place on TTY
                                  (separator-prefixed append on non-TTY).
                                  With --json, emits one compact JSON
                                  document per snapshot per line — jq-pipeable.
                                  Ctrl-C to exit. (Also: balanze-cli status
                                  --watch.)
balanze-cli setup                 Interactive wizard — run this first
balanze-cli set-openai-key        Store an sk-admin-… key in the OS keychain
                                  (masked TTY prompt, or piped stdin)
balanze-cli clear-openai-key      Remove the OpenAI key from the keychain
balanze-cli settings              Print current settings.json
balanze-cli statusline            Claude Code statusLine command: reads the
                                  statusLine JSON on stdin, prints a one-line
                                  status (live 5h/7d quota + session cost).
                                  Also atomically writes the snapshot file
                                  <ProjectDirs.data>/statusline.snapshot.json
                                  — the IPC bridge the v0.2 watcher reads.
balanze-cli help                  This help

Env override: BALANZE_OPENAI_KEY=sk-admin-...  (takes precedence over the
keychain; handy for CI/headless or a locked keychain).
```

`balanze-cli statusline` is Claude Code's statusLine command (offered by `balanze-cli setup`) — shows live 5h/7d subscription quota + session cost in your shell; zero-auth, no rate limit.

```text
=== Balanze status (2026-05-20 04:27:42 UTC) ===

                    Quota %                                 API $ (real billed)
Anthropic           ✓ 82.0% 5h, 88.0% 7d (oauth)            $20.92/$25.00 overage (real)
OpenAI              ✓ 6.0% 7d (codex go)                    $4.20 (admin costs)

Pace: 5h 82% used / 60% elapsed (1.4×);  7d 88% used / 95% elapsed (0.9×)
Subscription leverage: ~$2197.11 of Claude Code usage at API list prices (leverage — NOT billed)

Quota % = live server-reported utilization. API $ = real billed spend
only: Anthropic = pay-as-you-go overage (n/a unless enabled); OpenAI =
Admin Costs API. 'Subscription leverage' is a separate
list-price estimate, never charged.
```

(Without "Extra usage" enabled, the Anthropic API-$ cell reads `— not available` and only the leverage line carries a Claude dollar figure.)

`--sections` expands each source (cadence bars + reset clocks, the per-model JSONL breakdown + burn rate, the estimated-cost detail with LiteLLM provenance, the Codex window, OpenAI spend by line item). `--json` emits a machine-readable document with a top-level `schema_version` (currently `1`); every money cell is `{ value_micro_usd, source, confidence, details }` (OpenAI spend included - i64 micro-USD throughout), so a script can read `.anthropic_api_cost.value_micro_usd` and `.openai.value_micro_usd` uniformly and tell `jsonl_list_price`/`estimate` apart from `openai_admin_costs`/`real` without parsing labels. Two extra cells (v0.2): `.claude_statusline` carries the live `StatuslineFilePayload` envelope (`captured_at` + `payload.rate_limits` + `payload.cost.total_cost_usd` — Claude Code's *session* estimate, an explicitly distinct cost tier), and `.pace` carries a per-window array of `{ key, used_fraction, elapsed_fraction, ratio }` — used % vs elapsed % of each quota window (5h, 7d) plus their ratio, computed by `window::pace`; `ratio` is null right after a window reset. `--json -v` adds account identifiers (`org_uuid`, Codex `session_id`); without `-v` they're redacted. `--watch --json` reuses this same DTO, emitted one JSON object per line.

## Install

Balanze currently ships **from source only** — no binaries, installers, or GitHub Releases, and it is **not on crates.io** (signed binaries, Homebrew, and WinGet are on the roadmap; see [`docs/PRD.md`](docs/PRD.md)). Requires Rust 1.85+.

```bash
# `--git` is required (not on crates.io). The repo root is a virtual
# workspace, so name the package explicitly — it builds the `balanze-cli`
# binary. Plain `cargo install balanze_cli` will NOT work.
cargo install --git https://github.com/Oszkar/balanze balanze_cli
balanze-cli setup      # run this first — wizard for the OpenAI admin key
balanze-cli            # 4-quadrant status
```

**The CLI has zero system-library dependencies** — Windows 11, macOS 15+, and Linux build with just the Rust toolchain (Linux also needs a C compiler for the `ring` TLS dependency). No GTK/GLib/Cairo/WebKit — that native stack belongs to the desktop app, not the CLI.

The Claude side reads Claude Code's OAuth credential directly - no setup needed if Claude Code is already configured. It uses `~/.claude/.credentials.json` (or `~/.config/claude/.credentials.json`) where present, and on recent macOS falls back to Claude Code's login Keychain entry (read-only; macOS may prompt once to allow access). Provide the OpenAI Admin key via `balanze-cli setup`, `set-openai-key`, or the `BALANZE_OPENAI_KEY` env var.

## Develop

Prerequisites: Rust 1.85+ (all you need for the CLI); Bun 1.3+ (only for the Svelte popover frontend / `tauri dev`). Local builds use the Rust 1.94.0 toolchain pinned in `rust-toolchain.toml` (rustup picks it up automatically; CI uses the same version), and the repo pins Bun 1.3.13 via the `packageManager` field in `package.json`.

```bash
# CLI from the workspace:
cargo run --release -p balanze_cli -- status

# Full workspace checks:
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
bun run check                                  # svelte-check + tsc

# Desktop app — gauge tray icon + live popover (v0.3.0):
bun install                                    # also installs git hooks (see below)
bun run tauri dev
```

`bun install` runs `lefthook install` (skipped without `.git/`), wiring `commit-msg` (Conventional Commits — blocking), `pre-commit` (rustfmt + svelte-check) and `pre-push` (clippy + tests) so the gates CI enforces fail locally first. Bypass one commit with `git commit --no-verify`, or `LEFTHOOK=0` for a session.

**`default-members = ["crates/*"]`:** bare `cargo build`/`test`/`run` skip `src-tauri`, so a CLI build never needs GUI libraries. The desktop app is the explicit opt-in (`cargo build --workspace` or `bun run tauri dev`) and pulls in the platform GUI stack:

- **Windows:** WebView2 runtime + VS Build Tools (no GTK — Tauri uses WebView2).
- **macOS:** Xcode Command Line Tools.
- **Debian/Ubuntu:** `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev build-essential libssl-dev libglib2.0-dev pkg-config`

**Not needed for the CLI** — if you only want the CLI on Linux, never run a `--workspace` build and you'll never see a `gdk-3.0`/`pango`/`cairo` error. Test discipline and the per-crate validation matrix live in `AGENTS.md` §6–§7; the crate map and boundaries live in `docs/ARCHITECTURE.md`.

## Contributing

Not actively soliciting contributions yet — this is a personal tool first. Found a bug or want to discuss design? Open an issue. Sending a PR anyway? Read `AGENTS.md` and `docs/ARCHITECTURE.md` first; they codify the architectural boundaries and code-discipline rules.

## License

MIT — see `LICENSE`.
