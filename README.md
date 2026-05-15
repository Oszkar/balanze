# Balanze

A local-first utility that consolidates personal AI usage into one normalized
view — Claude subscription quota, an estimate of Claude Code's API-rate value,
OpenAI Codex quota, and real OpenAI API spend, in one glance. Rust + Tauri 2 +
Svelte 5. Side project; Windows 11 and macOS 15+ (CLI also runs on Linux).

## Status (v0.1 — "Data")

v0.1's bar is a **complete, honest data layer** exposed as a CLI
(`balanze-cli`). The tray UI is deliberately later (v0.3); the CLI prints the
same normalized snapshot the eventual popover will show. The four-quadrant
matrix lights up:

|            | Quota %                                   | API $                                              |
|------------|-------------------------------------------|----------------------------------------------------|
| **Anthropic** | ✅ OAuth usage (5h / 7-day / per-model) | ✅ estimated list-price from JSONL (**not** billed) |
| **OpenAI**    | ✅ Codex CLI rate-limit %               | ✅ real billed spend (Admin Costs API)              |

- ✅ **Anthropic OAuth usage** — the same `/api/oauth/usage` endpoint Claude
  Code uses; live 5-hour / 7-day / per-model utilization bars + `resets_at`
  clocks. No scraping.
- ✅ **Anthropic API $ (estimated)** — `claude_cost` synthesizes a list-price
  equivalent from local JSONL × a vendored LiteLLM price table. For Pro/Max
  users this is **subscription leverage, not money billed** — the CLI labels
  it that way. Anthropic's real cost API is enterprise-gated (see Known
  issues); this estimate is the honest best-available signal.
- ✅ **OpenAI Codex quota** — reads the local Codex CLI rollout files
  (`~/.codex/sessions/`) for the server-computed `rate_limits.primary` %.
- ✅ **OpenAI Admin Costs** — `/v1/organization/costs` with an `sk-admin-…`
  key; this-month spend + per-line-item breakdown. Real billing data.
- ✅ **`balanze-cli setup`** — interactive wizard: checks Anthropic OAuth +
  Codex presence, prompts for the OpenAI admin key (masked), validates it
  live, stores it.
- 🛣️ **v0.2 — Liveness**: file watcher, predictor (EWMA + warm-up), `--watch`,
  `statusline`.
- 🛣️ **v0.3 — UI**: Tauri tray + popover, settings UI, keychain v4 migration,
  alerts, Anthropic Console cookie-paste.
- 🛣️ **v0.4 — Distribution**: signed binaries, Homebrew, WinGet, auto-update.

Phasing detail: `docs/prd.md`. Architecture and discipline: `AGENTS.md`.

## CLI

`balanze-cli` is the v0.1 surface and the reference composition for the
eventual tray popover. Subcommands:

```text
balanze-cli                       4-quadrant compact status (default)
balanze-cli status [--json] [--sections] [-v]
                                  --sections  per-source detail (cadence bars,
                                              model breakdown, codex window)
                                  --json      machine-readable Snapshot JSON
                                              (wins over --sections if both)
                                  -v          adds account-identifying fields
                                              (org uuid, codex session_id)
balanze-cli setup                 Interactive wizard — run this first
balanze-cli set-openai-key [KEY]  Store an sk-admin-… key in the OS keychain
balanze-cli clear-openai-key      Remove the OpenAI key from the keychain
balanze-cli settings              Print current settings.json
balanze-cli help                  This help

Env override: BALANZE_OPENAI_KEY=sk-admin-…  (takes precedence over the
keychain; recommended on Windows until the keyring-v4 migration in v0.3).
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

Run `balanze-cli --sections` for per-source detail, or `balanze-cli --json` for machine-readable output.
```

`--sections` expands each source: Anthropic cadence bars with reset clocks, the
per-model JSONL breakdown + burn rate, the estimated-cost detail (with the
LiteLLM price-table provenance), the Codex window, and OpenAI spend by line
item. `--json` emits the full `Snapshot` for scripting.

## Install (v0.1)

v0.1 ships **from source only** — no binaries, no installers, no GitHub
Releases. The audience (tinkerer power-users) accepts the Rust-toolchain
prerequisite; signed binaries / Homebrew / WinGet are the v0.4 Distribution
phase. Requires Rust 1.77+.

```bash
# The repo root is a virtual workspace, so name the package (balanze_cli);
# it builds the `balanze-cli` binary.
cargo install --git https://github.com/Oszkar/balanze balanze_cli
balanze-cli setup      # run this first — wizard for the OpenAI admin key
balanze-cli            # 4-quadrant status
```

Works on Windows 11, macOS 15+, and Linux (no separate Linux test matrix; the
tray UI is a later phase anyway).

## Quick start (dev)

Prerequisites:

- Rust 1.77+ (workspace MSRV)
- Bun 1.3+ (for the Svelte frontend scaffold)
- Platform build tools — Windows: WebView2 + VS Build Tools; macOS: Xcode CLI tools

```bash
# CLI from the workspace:
cargo run --release -p balanze_cli -- status

# Desktop app (scaffold only — tray icon, no data yet; the real UI is v0.3):
bun install         # also installs git hooks — see "Dev tooling" below
bun run tauri dev
```

### Dev tooling

`bun install` runs `lefthook install` automatically (skipped when there's no
`.git/` — e.g., source tarballs). That wires `commit-msg` (Conventional
Commits — blocking), `pre-commit` (rustfmt + svelte-check), and `pre-push`
(clippy + tests) hooks so the same gates CI enforces fail locally first. If
you edit `lefthook.yml`, re-run `bun run lefthook install` to sync the hooks.
Bypass for one commit with `git commit --no-verify`, or `LEFTHOOK=0` for one
session.

Provide an OpenAI Admin key one of two ways:

```bash
# Recommended on Windows (keychain backend currently unreliable, see Known issues):
BALANZE_OPENAI_KEY=sk-admin-... cargo run --release -p balanze_cli

# Or store in the OS keychain (macOS works today):
cargo run --release -p balanze_cli -- set-openai-key sk-admin-...
```

The Claude side reads `~/.claude/.credentials.json` directly — no setup needed
if Claude Code is already configured.

## Build (release)

```bash
# CLI (the v0.1 deliverable):
cargo build --release -p balanze_cli   # → target/release/balanze-cli

# Desktop app (scaffold; the real UI lands in v0.3):
bun run tauri build
```

The `release.yml` workflow + Tauri bundling (`.msi`/`.exe`, `.dmg`/`.app`)
exist but are **forward-looking** — v0.1 is source-install only. Signed,
packaged binaries are the v0.4 Distribution phase.

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
│   ├── anthropic_oauth/      Anthropic /api/oauth/usage client + credentials
│   ├── openai_client/        OpenAI /v1/organization/costs client
│   ├── codex_local/          reads ~/.codex/sessions/ for Codex rate-limit %
│   ├── window/               pure rolling-window math (5h + 30m burn rate)
│   ├── state_coordinator/    actor crate; owns Snapshot, notifies Sink
│   ├── settings/             non-secret settings.json (atomic write)
│   ├── keychain/             OS keychain wrapper (only consumer of `keyring`)
│   └── balanze_cli/          CLI entry-point composing the backend crates
├── docs/prd.md               product spec + phasing
├── AGENTS.md                 operational contract for AI agents / contributors
└── .github/workflows/        CI (Win+Mac) + release matrix
```

## Known issues

- **Keychain backend broken on Windows (v0.1).** `keyring = "3.6.3"` silently
  no-ops: `set_password` returns `Ok` but the credential never lands in
  Credential Manager. Workaround: set `BALANZE_OPENAI_KEY` env var. Fix
  scheduled for **v0.3** (the `keyring` → `keyring-core` v4 migration, which
  rides with the settings UI where the key-input box exercises it on both
  platforms). Detail: `AGENTS.md` §10a.

- **Anthropic OAuth bearer expires every ~7–8h.** Today the CLI surfaces this
  as an `AuthExpired` error; re-run `claude login` and retry. Refresh-token
  flow is v0.1.1 work.

- **`extra_usage` block from OAuth suppressed.** Anthropic's OAuth response
  returns a `monthly_limit / used_credits` block whose semantics don't
  reconcile with the claude.ai/settings/usage UI. Suppressed in pretty CLI
  output until the v0.3 Anthropic Console (HAR) investigation resolves the
  meaning; raw values are still in `--json` for diagnostics.

## Testing

```bash
cargo test --workspace                              # full workspace suite
cargo clippy --workspace --all-targets -- -D warnings
bun run check                                       # svelte-check + tsc
```

Test discipline + per-crate coverage live in `AGENTS.md` §6 (validation matrix)
and §7 (test discipline).

## Contributing

Not actively soliciting contributions yet — this is a personal tool first. If
you find a bug or want to discuss design, open an issue. If you want to send a
PR anyway: read `AGENTS.md` first; it codifies the architectural boundaries
and code-discipline rules.

## License

MIT — see `LICENSE`.

## Not affiliated

Balanze is a personal tool. Not affiliated with, endorsed by, or sponsored by
Anthropic or OpenAI. It only reads endpoints and files the user already has
access to with their own credentials.
