# Balanze

A local-first desktop utility that consolidates personal AI usage into one view —
Claude subscription caps, Claude Code activity, and OpenAI API spend in the same
glance. Tray-first; Tauri 2 + Rust + Svelte 5. Side project; Windows 11 and macOS
15+ today, Linux and mobile later.

## Status (v0.1, May 2026)

The backend data layer is shipped. The desktop tray UI is still scaffold; the
working surface today is a CLI (`balanze`) that emits the same status snapshot
the tray popover will eventually show.

- ✅ **Anthropic OAuth usage** — calls the same `/api/oauth/usage` endpoint
  Claude Code uses; reports the live 5-hour / 7-day / per-model utilization bars
  and `resets_at` clocks. No scraping.
- ✅ **Claude Code JSONL** — incremental parse of `~/.claude/projects/**/*.jsonl`
  with `(message_id, request_id)` dedup. On a real session, raw lines overcounted
  tokens by ~50%; dedup brings the numbers in line with what Anthropic sees.
- ✅ **OpenAI Admin Costs** — calls `/v1/organization/costs` with an
  `sk-admin-…` admin key; reports this-month spend and per-line-item breakdown.
- 🚧 **Tauri tray + popover UI** — designed in `docs/prd.md`; integration with
  the actor-based `state_coordinator` is next.
- 🛣️ **v0.2** — Anthropic Console scrape, alerts, history graph, refresh-token
  flow, keychain v4 migration.
- 🛣️ **v0.3+** — cross-device sync, Android, hosted wallboard.

Phasing detail: `docs/prd.md`. Architecture and discipline: `AGENTS.md`.

## CLI

The CLI is the v0.1 reference for the eventual tray popover. Same composition,
just stdout instead of an OS tray.

```text
$ balanze help
Balanze — local-first AI usage tracker.

Subcommands:
  balanze                       Print pretty status (default)
  balanze status [--json]       Same as above; --json is machine-readable
  balanze set-openai-key [KEY]  Store KEY in the OS keychain
  balanze clear-openai-key      Remove the OpenAI key from the keychain
  balanze settings              Print current settings.json contents
  balanze help                  This help

Environment overrides:
  BALANZE_OPENAI_KEY            sk-admin-… admin key. Takes precedence over
                                keychain. Recommended on Windows until v0.2.
```

Typical output:

```text
=== Balanze Status ===
fetched: 2026-05-14 12:34:56 UTC

subscription: max (pro)

CADENCE BARS (from Anthropic OAuth):
  Current 5-hour session             42.30%   resets in 2h 18m
  Sonnet only (7 days)               18.05%   resets in 5d 13h
  Opus only (7 days)                  3.10%   resets in 5d 13h

OPENAI SPEND (2026-05-01 – 2026-05-14):
  Total: $4.21

  By line item:
    gpt-5                            $    3.9100
    whisper                          $    0.2100
    embeddings                       $    0.0900

CLAUDE CODE ACTIVITY (last 5h, from local JSONL):
  files scanned:     463
  events in window:  48
  tokens in window:  10,783,227
  recent burn:       ~38,420 tokens/min (last 30 min)

  By model:
    claude-opus-4-7                       events:   30  tokens:   8,210,940
    claude-sonnet-4-6                     events:   15  tokens:   2,431,290
    claude-haiku-4-5                      events:    3  tokens:     140,997
```

## Quick start (dev)

Prerequisites:

- Rust 1.77+ (workspace MSRV)
- Bun 1.3+ (for the Svelte frontend)
- Platform build tools — Windows: WebView2 + VS Build Tools; macOS: Xcode CLI tools

```bash
# CLI (works today):
cargo run --release -p balanze_cli -- status

# Desktop app (scaffold only — tray icon, no data yet):
bun install
bun run tauri dev
```

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
# CLI:
cargo build --release -p balanze_cli

# Desktop app:
bun run tauri build
```

Tauri bundles land in `src-tauri/target/release/bundle/`:

- Windows: `.msi` and `.exe` (NSIS)
- macOS: `.dmg` and `.app`

CI matrix-builds both on tags `v*.*.*`; see `.github/workflows/release.yml`.

## Layout

```
balanze/
├── Cargo.toml                workspace root (Rust 2021, MSRV 1.77)
├── package.json              bun + Svelte 5 + TypeScript + Vite
├── src/                      Svelte frontend (scaffold today)
├── src-tauri/                Tauri 2 app crate (scaffold tray + single-instance)
├── crates/
│   ├── claude_parser/        JSONL parser + walker + dedup + IncrementalParser
│   ├── anthropic_oauth/      Anthropic /api/oauth/usage client + credentials
│   ├── openai_client/        OpenAI /v1/organization/costs client
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
  scheduled for v0.2 via migration to `keyring-core`. Detail: `AGENTS.md` §10a.

- **Anthropic OAuth bearer expires every ~7–8h.** Today the CLI surfaces this
  as an `AuthExpired` error; re-run `claude login` and retry. Refresh-token
  flow is v0.1.1 work.

- **`extra_usage` block from OAuth suppressed.** Anthropic's OAuth response
  returns a `monthly_limit / used_credits` block whose semantics don't
  reconcile with the claude.ai/settings/usage UI. Suppressed in pretty CLI
  output until a v0.2 HAR investigation resolves the meaning; raw values are
  still in `--json` for diagnostics.

## Testing

```bash
cargo test --workspace                              # 117 tests, ~5s
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
