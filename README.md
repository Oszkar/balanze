<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logo-white.svg">
    <img src="docs/assets/logo.svg" alt="Balanze" width="140">
  </picture>
</p>

<h1 align="center">Balanze</h1>

<p align="center">
  <a href="https://github.com/Oszkar/balanze/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/Oszkar/balanze/ci.yml?branch=main&label=ci&logo=github"></a>
  <a href="https://github.com/Oszkar/balanze/tags"><img alt="Version" src="https://img.shields.io/github/v/tag/Oszkar/balanze?label=version&color=blue"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
  <a href="Cargo.toml"><img alt="Rust" src="https://img.shields.io/badge/rust-1.85%2B-orange?logo=rust&logoColor=white"></a>
</p>

A local-first utility that consolidates personal AI usage into one normalized view - Claude subscription quota, an estimate of Claude Code's API-rate value, OpenAI Codex quota, and real OpenAI API spend, in one glance. Rust + Tauri 2 + Svelte 5. Side project; Windows 11 and macOS 15+ (CLI also runs on Linux).

> Not affiliated with, endorsed by, or sponsored by Anthropic or OpenAI. Reads only endpoints and files you already have access to with your own credentials.

## What it does

A **complete, honest data layer** surfaced two ways - the `balanze-cli` CLI and a **tray popover** (a color-shifting gauge tray icon, a glanceable grid/cards view, and a settings panel for keys + provider toggles). Both render the same normalized snapshot. The matrix holds **measured reality only** - server-reported quota % and real billed $ - so cells in a column are always the same *kind* of number:

|               | Quota %                              | API $ (real billed)                                 |
|---------------|--------------------------------------|-----------------------------------------------------|
| **Anthropic** | OAuth usage (5h / 7-day / per-model) | `extra_usage` overage if you enabled it, else *n/a* |
| **OpenAI**    | Codex CLI rate-limit %               | real billed spend (Admin Costs API)                 |

The JSONL list-price estimate is **not** a matrix cell - it's a separate **"Subscription leverage"** insight (see below).

- **Anthropic quota** - the same `/api/oauth/usage` endpoint Claude Code uses: live 5-hour / 7-day / per-model bars + `resets_at` clocks. No scraping.
- **Anthropic API $ - real or nothing.** Anthropic exposes no per-user API spend, so this cell shows the real `extra_usage` pay-as-you-go overage *if* you enabled it on claude.ai (the same billed cents claude.ai's overage meter shows), and otherwise reads as **not available** - never backfilled with a substitute number.
- **Subscription leverage (estimate, separate)** - `claude_cost` synthesizes a list-price equivalent from local JSONL × a vendored LiteLLM price table: what your Claude Code usage *would* cost at API list prices. For Pro/Max users this is **leverage from the subscription, never billed** - so it's presented as its own insight, outside the matrix, where it can't be mistaken for spend.
- **OpenAI Codex quota** - reads the local Codex CLI rollout files (`~/.codex/sessions/`) for the server-computed `rate_limits.primary` %.
- **OpenAI API $** - `/v1/organization/costs` with an `sk-admin-...` key: this-month spend + per-line-item breakdown. Real billing data.

Planning and history live elsewhere: roadmap and phase detail in [`docs/PRD.md`](docs/PRD.md); architecture and boundaries in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md); release history in [`CHANGELOG.md`](CHANGELOG.md); code discipline in [`AGENTS.md`](AGENTS.md); common gotchas in [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md); security posture in [`docs/SECURITY.md`](docs/SECURITY.md).

## CLI

`balanze-cli` is the headless surface and the reference composition the tray popover renders. It is a clap-derive multi-command tool; bare `balanze-cli` (no subcommand) defaults to `status`. (Upgrading from an earlier `--git` build? The old bare flags are now subcommands - `--json` / `--sections` moved under `status`, and `--watch` became `watch`.)

<p align="center">
  <img src="docs/assets/watch-tui.png" alt="balanze-cli watch - live cross-provider TUI" width="640">
</p>
<p align="center"><sub><code>balanze-cli watch</code> - a live, bounded TUI showing Anthropic and OpenAI usage side by side.</sub></p>

```text
balanze-cli                       4-quadrant compact status (default; colored on
                                  a TTY, honors NO_COLOR / --no-color)
balanze-cli status [--json] [--sections]
                                  --sections  per-source detail (cadence bars,
                                              model breakdown, codex window)
                                  --json      machine-readable Snapshot JSON
                                              (wins over --sections if both)
balanze-cli watch [--json]        Live view. On an interactive TTY without --json
                                  it draws a bounded ratatui TUI (q / Esc /
                                  Ctrl-C to quit). Piped/redirected or with
                                  --json it streams instead: non-TTY appends the
                                  compact view separator-prefixed; --json emits
                                  one compact JSON document per snapshot per line
                                  (jq-pipeable).
balanze-cli doctor [--offline]    Diagnose each integration (six probes):
                                  OK/WARN/FAIL per source + an actionable hint +
                                  a one-line summary. --offline skips the network
                                  validation of the OpenAI key. The exit code
                                  reflects the worst finding (see Exit codes).
balanze-cli export [-o <file>]    Stateless CSV of usage history, re-derived from
                                  JSONL each run (nothing persisted). Claude
                                  (day, model) rows carry a list-price leverage
                                  column (jsonl_list_price, estimate); OpenAI
                                  current-month billed rows (openai_admin_costs,
                                  real) sit in a SEPARATE column, never summed.
                                  -o writes a file instead of stdout.
balanze-cli completions <shell>   Print a shell completion script to stdout
                                  (bash, zsh, fish, powershell, elvish).
balanze-cli setup                 Interactive wizard - run this first
balanze-cli set-openai-key        Store an sk-admin-... key in the OS keychain
                                  (masked TTY prompt, or piped stdin)
balanze-cli clear-openai-key      Remove the OpenAI key from the keychain
balanze-cli settings              Print current settings.json
balanze-cli statusline            Claude Code statusLine command: reads the
                                  statusLine JSON on stdin, prints a one-line
                                  status (live 5h/7d quota + session cost).
                                  Also atomically writes the snapshot file
                                  <ProjectDirs.data>/statusline.snapshot.json -
                                  the IPC bridge the watcher reads.
balanze-cli help [cmd]            Built-in clap help (also --help / -h).

Global flags (apply to every subcommand, before or after it):
  -v, --verbose   Surface account-identifying fields (org_uuid, codex
                  session_id); redacted without it.
      --quiet     Suppress non-essential output: drops the status compact matrix
                  (not --json) and trims doctor to WARN/FAIL lines.
      --no-color  Disable ANSI color (NO_COLOR is also honored; non-TTY
                  auto-disables).
      --strict    Treat a degraded source (stale/errored) as failure: exit 5
                  instead of 0.
  -V, --version   Print the version and exit.

A hidden `man` subcommand prints the man-page roff to stdout; a build.rs also
renders the completions + man page into OUT_DIR for packaging.

Env override: BALANZE_OPENAI_KEY=sk-admin-...  (takes precedence over the
keychain; handy for CI/headless or a locked keychain).
```

**Exit codes** (for scripting). `main` classifies the outcome once, and `doctor` shares the same taxonomy:

| Code | Meaning |
|------|---------|
| 0 | OK (a degraded source still exits 0 unless `--strict`) |
| 1 | unexpected / other error |
| 2 | usage error (bad flags / unknown subcommand; clap owns this) |
| 3 | auth: credentials missing or expired (re-run `claude login`, or set the OpenAI key) |
| 4 | network: a provider was unreachable |
| 5 | degraded: a source was stale or errored (only with `--strict`) |

`balanze-cli statusline` is Claude Code's statusLine command (offered by `balanze-cli setup`) - shows live 5h/7d subscription quota + session cost in your shell; zero-auth, no rate limit.

```text
=== Balanze status (2026-05-20 04:27:42 UTC) ===

                    Quota %                                 API $ (real billed)
Anthropic           ✓ 82.0% 5h, 88.0% 7d (oauth)            $20.92/$25.00 overage (real)
OpenAI              ✓ 6.0% 7d (codex go)                    $4.20 (admin costs)

Pace: 5h 82% used / 60% elapsed (1.4×);  7d 88% used / 95% elapsed (0.9×)
Subscription leverage: ~$2197.11 of Claude Code usage at API list prices (leverage - NOT billed)

Quota % = live server-reported utilization. API $ = real billed spend
only: Anthropic = pay-as-you-go overage (n/a unless enabled); OpenAI =
Admin Costs API. 'Subscription leverage' is a separate
list-price estimate, never charged.
```

(Without "Extra usage" enabled, the Anthropic API-$ cell reads `- not available` and only the leverage line carries a Claude dollar figure.)

`status --sections` expands each source: cadence bars + reset clocks, the per-model JSONL breakdown + burn rate, the leverage-cost detail with LiteLLM provenance, the Codex window, and OpenAI spend by line item. `status --json` emits a machine-readable document keyed by a top-level `schema_version`; every money cell is tagged `{ value_micro_usd, source, confidence, details }` (i64 micro-USD throughout, OpenAI included), so a consumer tells an `estimate` apart from `real` billed spend straight from the wire shape - no label parsing. `watch --json` streams the same DTO, one JSON object per line. Add `-v` to `status` for account identifiers (`org_uuid`, Codex `session_id`), redacted by default. The full schema - including the `pace` and `claude_statusline` cells - is documented in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

**Shell completions.** `balanze-cli completions <shell>` prints a script to stdout; install it per shell, for example:

```bash
balanze-cli completions bash > ~/.local/share/bash-completion/completions/balanze-cli
balanze-cli completions zsh  > "${fpath[1]}/_balanze-cli"
balanze-cli completions fish > ~/.config/fish/completions/balanze-cli.fish
balanze-cli completions powershell >> $PROFILE
```

## Install

Balanze currently ships **from source only** - no binaries, installers, or GitHub Releases, and it is **not on crates.io** (signed binaries, Homebrew, and WinGet are on the roadmap; see [`docs/PRD.md`](docs/PRD.md)). Requires Rust 1.85+.

```bash
# `--git` is required (not on crates.io). The repo root is a virtual
# workspace, so name the package explicitly - it builds the `balanze-cli`
# binary. Plain `cargo install balanze_cli` will NOT work.
cargo install --git https://github.com/Oszkar/balanze balanze_cli
balanze-cli setup      # run this first - wizard for the OpenAI admin key
balanze-cli            # 4-quadrant status
```

**The CLI has zero system-library dependencies** - Windows 11, macOS 15+, and Linux build with just the Rust toolchain (Linux also needs a C compiler for the `ring` TLS dependency). No GTK/GLib/Cairo/WebKit - that native stack belongs to the desktop app, not the CLI.

The Claude side reads Claude Code's OAuth credential directly - no setup needed if Claude Code is already configured. It uses `~/.claude/.credentials.json` (or `~/.config/claude/.credentials.json`) where present, and on recent macOS falls back to Claude Code's login Keychain entry (read-only; macOS may prompt once to allow access). Provide the OpenAI Admin key via `balanze-cli setup`, `set-openai-key`, the popover's settings panel, or the `BALANZE_OPENAI_KEY` env var.

## Develop

Prerequisites: Rust 1.85+ (all you need for the CLI); Bun 1.3+ (only for the Svelte popover frontend / `tauri dev`). Local builds use the Rust 1.94.0 toolchain pinned in `rust-toolchain.toml` (rustup picks it up automatically; CI uses the same version), and the repo pins Bun 1.3.13 via the `packageManager` field in `package.json`.

```bash
# CLI from the workspace:
cargo run --release -p balanze_cli -- status

# Full workspace checks:
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
bun run check                                  # svelte-check + tsc

# Desktop app - gauge tray icon + live popover:
bun install                                    # also installs git hooks (see below)
bun run tauri dev

# States gallery (dev-only) - every screen and cell state on one canvas:
bun run dev                                    # then open http://localhost:1420/gallery
```

**States gallery (dev-only).** `bun run dev` and open <http://localhost:1420/gallery> to see every popover screen and cell state on one page - cold-start loading, the OpenAI connect CTA, fetch errors, stale windows, single vs two providers, billed overage, and the settings panel - in both light and dark, rendered with the real Svelte components and `theme.css` tokens. It runs in a plain browser with no Tauri host (IPC is stubbed and every write is a no-op, so it can't touch your keychain or settings) and is stripped from production builds. Handy for visual QA and screenshots without reproducing provider failures live. Source: `src/routes/gallery/` + `src/lib/gallery/`.

`bun install` runs `lefthook install` (skipped without `.git/`), wiring `commit-msg` (Conventional Commits - blocking), `pre-commit` (rustfmt + svelte-check) and `pre-push` (clippy + tests) so the gates CI enforces fail locally first. Bypass one commit with `git commit --no-verify`, or `LEFTHOOK=0` for a session.

**`default-members = ["crates/*"]`:** bare `cargo build`/`test`/`run` skip `src-tauri`, so a CLI build never needs GUI libraries. The desktop app is the explicit opt-in (`cargo build --workspace` or `bun run tauri dev`) and pulls in the platform GUI stack:

- **Windows:** WebView2 runtime + VS Build Tools (no GTK - Tauri uses WebView2).
- **macOS:** Xcode Command Line Tools.
- **Debian/Ubuntu:** `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev build-essential libssl-dev libglib2.0-dev pkg-config`

**Not needed for the CLI** - if you only want the CLI on Linux, never run a `--workspace` build and you'll never see a `gdk-3.0`/`pango`/`cairo` error.

### Finding your way around

The workspace is a set of small, single-responsibility crates under `crates/`: one HTTP client per provider, one keychain wrapper, one actor that owns state. The twelve boundaries that keep them honest are spelled out in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md); the short version:

- **Provider connectors** - `anthropic_oauth`, `openai_client`, `codex_local`, and `claude_parser` (the Claude JSONL wire format) each own one source. Adding a provider means a new connector crate wired into the `SnapshotSources` fetches that `snapshot_composer::compose` orchestrates (plus the watcher/coordinator for live updates) - the normalized `Snapshot` and the actor stay put. That connector abstraction is the design's central bet.
- **Domain math** - `window` (rolling-window + pace) and `claude_cost` (the pure list-price estimate). Pure functions, no I/O, tested first.
- **Composition + glue** - `snapshot_composer` (one-shot) and `state_coordinator` (the live actor) both assemble the same `Snapshot`; `balanze_cli` and `src-tauri` are thin glue over them, never logic.

Hitting a wall? [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md) collects the non-obvious traps (double tray icons, JSONL CPU spikes, Tauri dep-version mismatches). Test discipline and the per-crate validation matrix live in `AGENTS.md` §6-§7.

## Contributing

Not actively soliciting contributions yet - this is a personal tool first. Found a bug or want to discuss design? Open an issue. Sending a PR anyway? Read `AGENTS.md` and `docs/ARCHITECTURE.md` first; they codify the architectural boundaries and code-discipline rules.

## License

MIT - see `LICENSE`.
