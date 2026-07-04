# v0.4.2 "Statusline" - Design

Status: approved (design), pre-implementation
Date: 2026-06-30
Theme (from `docs/PRD.md`): productize the Claude Code statusline into a first-class, installable, cross-provider display that replaces any existing statusline.

## 1. Goal and framing

v0.2 shipped `balanze-cli statusline` as the watcher's internal IPC bridge: it parses Claude Code's `statusLine` stdin payload, prints a plain one-liner (`bal 5h 13% . 7d 44% . sess-est $12.50`), and atomically writes `statusline.snapshot.json` for the watcher. It is Claude-only, uncolored, unconfigurable, and never installed as the user's real statusline.

v0.4.2 turns that bridge into a display the user actually runs in their coding-agent prompt:

- A cross-provider one-liner: Claude 5h/7d quota alongside Codex quota and real billed OpenAI spend - the differentiator no Claude-only incumbent (ccusage, cship, claude-powerline) can show, because Balanze already computes all of it.
- Replace, not wrap: detect and replace any existing `statusLine.command` (cship is the motivating example, but the flow is provider-agnostic) so the user can switch with consent and switch back.
- A configurable display: segment selection, per-segment styles and thresholds, light/dark - the ~80% of a statusline that is presentation, not novel data.
- A machine-wide OpenAI fallback cache so the line is cheap and stays polite (AGENTS.md 3.1) even though it runs on every conversation turn.
- Codex stays a config preset, not a build (no external statusline hook exists; Codex shows usage natively).

This document is the design contract. The blow-by-blow implementation plan (tasks, ordering, verify commands) is produced separately by the writing-plans step.

## 2. Resolved decisions

| # | Decision | Choice | Rationale |
|---|---|---|---|
| D1 | Where the logic lives | New crate `statusline_render` | `balanze_cli` is thin glue (never logic); `claude_statusline` is deliberately dependency-light (no networking). The segment model + renderer + cache + cross-provider composition is real logic and belongs in its own crate. Requires an `ARCHITECTURE.md` crate-map update (the crate set is otherwise fixed per AGENTS.md 2). |
| D2 | Cross-provider data sourcing | Hybrid | Read the running watcher/desktop snapshot when fresh; self-compose with a machine-wide OpenAI fallback cache otherwise. Works whether or not the desktop app runs, and never violates 3.1. |
| D3 | Segment set | Curated default, informed by the user's real cship config | Not exact cship parity; an opinionated, configurable default. cship had no "secret sauce" worth mirroring 1:1. |
| D4 | The Starship dev-context line | Dropped for v0.4.2 | Directory/git/language versions are not usage data and already live in the shell prompt and terminal. Reproducing them would pull Balanze toward being a prompt framework. Deferred to a GitHub issue (investigate shell-out to `starship prompt` vs a native minimal dir+git line). |
| D5 | Config storage | Extend `settings.json` with a `statusline` section | Reuses the existing atomic, versioned, `#[serde(default)]` loader; the v0.7 settings panel edits the same file. No second config format. |
| D6 | Default-on extra segments | Reset countdown, pace indicator, staleness marker | All free from stdin / already computed; honest and high-value. Everything else (effort, output_style, session duration, lines +/-, exceeds_200k, OpenAI today/month, burn rate) ships available-but-off via config. |
| D7 | Subscription leverage in the line | Not default-on; explicitly-labeled opt-in segment only | In a cramped one-liner next to real `OpenAI $`, a counterfactual list-price figure reads as spend - exactly the misread the measured-only matrix contract exists to prevent. |
| D8 | Replace flow scope | Generic (any foreign statusline), not cship-specific | `WireStatus::OccupiedBy(String)` already models the occupant generically; the flow backs up and restores the prior command verbatim and never touches the foreign tool's own config files. |
| D9 | Codex | Docs note + a print-only preset helper | No external statusline injection point (openai/codex#17827); Codex shows usage natively. `balanze-cli statusline --codex-preset` prints a recommended `[tui] status_line` snippet; the user pastes it. No auto-write. |
| D10 | Release slicing | 5 stacked PRs, review checkpoint at each | See section 9. Each PR is independently shippable and respects 3.1 at its own boundary. |

## 3. Architecture

### 3.1 New crate: `statusline_render`

Owns the segment model, the renderer (snapshot + config -> string), the OpenAI fallback cache, and the cross-provider self-compose path. `balanze-cli statusline` and a future desktop statusline surface stay thin over it.

Dependencies: `claude_statusline` (stdin parse + the new full-`Snapshot` IPC read), `settings` (the `statusline` config section), `codex_local` and `openai_client` (self-compose fallback), the crate that defines `Snapshot`, plus `chrono`/`serde`/`tracing`. It does NOT depend on `anthropic_oauth` (see 5.3).

### 3.2 Parser extension in `claude_statusline`

Today `parse` extracts `version`, `cost`, `rate_limits`. cship reads `model`, `agent`, `context_window`, `effort` from the same stdin payload, so we extend `parse` (and `StatuslineSnapshot`) to surface those. This stays in-crate and adds no networking. Exact stdin field names are verified against a live Claude Code payload and the existing `tests/fixtures/real-payload.json` during PR1 (real-data cross-reference per the project's QA discipline).

### 3.3 Data flow (per turn)

```
Claude Code  --stdin JSON-->  balanze-cli statusline  -->  statusline_render::render(stdin_snapshot, cross_provider, config)  -->  stdout (ANSI lines)
                                                  |                    ^
                                                  |                    |
                                       (always writes IPC          cross_provider =
                                        statusline.snapshot.json    fresh snapshot.json  OR  self-compose(codex_local + openai_client[cached])
                                        for the watcher, unchanged)
```

- Claude segments (model, agent, context, cost, 5h/7d quota, pace, reset countdown) come ALWAYS from stdin - zero-auth, authoritative during active use, never an OAuth call.
- Cross-provider segments (Codex %, OpenAI billed $) come from the Hybrid path (section 5).

## 4. Segment model, default layout, config schema

### 4.1 Default layout (two lines; Starship line dropped)

```
line 1:  model  agent
line 2:  context_bar  cost   5h%  pace  (reset)   7d%   Codex%   OpenAI $
```

Concretely, with default-on extras:

```
robot model  arrow agent
[ctx] $cost   5h 82% up1.4x (1h23m)  7d 88%  Codex 6%  OpenAI $4.20
```

Default thresholds match the user's cship taste so a switch loses nothing visually: context_bar warn 40 / critical 70, cost warn $2 / critical $5, usage warn 70 / critical 90. (These are defaults in config, freely overridable - Balanze's hardcoded 50/90 buckets in `present.rs`/`tauri_sink.rs` are NOT reused; see 8.)

### 4.2 Segments

Default-on: `model`, `agent` (rendered only when present), `context_bar`, `cost` (Claude session estimate, labeled as an estimate tier), `usage` (5h/7d with reset countdown + pace + staleness), `codex` (quota %, with staleness), `openai_cost` (real billed $, with staleness).

Available-but-off: `effort`, `output_style`, `session_duration`, `lines_changed`, `exceeds_200k` flag, `openai_today_vs_month`, `burn_rate`, `leverage` (the opt-in, explicitly-labeled list-price counterfactual).

The three default-on extras (D6):
- Reset countdown: derived from `rate_limits.*.resets_at`.
- Pace: used% vs window-elapsed%, where `elapsed = 1 - (resets_at - now) / window_len` and `window_len` is 5h / 7d. Reuses `window::pace`. Free from stdin, no extra source.
- Staleness marker: a small warning glyph on a cross-provider segment when its data is stale (no fresh snapshot and self-compose failed/unavailable), reusing the v0.3.1 degraded-state discipline. Honest over confidently-wrong.

### 4.3 Config schema (additive, in `settings.json`)

New `statusline` field on `Settings`, `#[serde(default)]`, no schema version bump:

```jsonc
"statusline": {
  "theme": "dark",                       // "dark" | "light"; auto-detect deferred
  "lines": [
    "{model} {agent}",
    "{context_bar} {cost} {usage} {codex} {openai_cost}"
  ],
  "segments": {
    "context_bar": { "width": 10, "warn": 40, "critical": 70, "style": "...", "warn_style": "...", "critical_style": "..." },
    "cost":        { "warn": 2.0, "critical": 5.0, "style": "...", "warn_style": "...", "critical_style": "..." },
    "usage":       { "warn": 70, "critical": 90, "show_pace": true, "show_reset": true, "style": "...", "warn_style": "...", "critical_style": "..." },
    "codex":       { "warn": 70, "critical": 90, "style": "...", "warn_style": "...", "critical_style": "..." },
    "openai_cost": { "style": "..." }
    // available-but-off segments accept the same shape when added to `lines`
  }
}
```

Styles use a minimal cship-like grammar (`bold fg:#7aa2f7`, `fg:#... bg:#...`, `italic`, `underline`) rendered to 24-bit ANSI by a small style parser in `statusline_render`. `theme` selects between a dark and a light default palette; explicit per-segment styles override the theme. Auto-detecting terminal background is out of scope (config-driven, dark default).

## 5. Hybrid sourcing and the OpenAI fallback cache

### 5.1 Fresh-read path (preferred; PR2)

The watcher and the desktop host atomically write a full-`Snapshot` IPC file - `<ProjectDirs.data>/snapshot.json` carrying the composed `Snapshot` + a `captured_at` timestamp + a `schema_version` - on each coordinator update. The statusline reads it and uses its Codex + OpenAI cells when `captured_at` is within a freshness TTL (proposed 120s; the watcher safety-poll floor is 60s). This is the IPC-contract addition (section 7), confirmed in design.

When present and fresh, the statusline does zero network: it reuses data the already-polite watcher produced.

### 5.2 Self-compose fallback (PR3)

When there is no fresh `snapshot.json` (desktop not running, or stale), the statusline composes cross-provider data itself:

- Codex: `codex_local` reads `~/.codex/sessions` (local file, cheap; read each turn, optionally micro-cached a few seconds to avoid re-parse on rapid turns).
- OpenAI billed $: `openai_client` calls the Admin Costs API through the machine-wide fallback cache.

### 5.3 The cache (PR3)

A machine-wide file cache under `<ProjectDirs.cache>/statusline/openai-cost.json` (or the `BALANZE_CACHE_DIR_OVERRIDE` equivalent in tests), not written next to the user's transcript:

- OpenAI costs: 300s TTL (this IS the 3.1 5-minute politeness gate), stale-while-updating so the line never blanks, a 300s negative-failure cooldown so a failing API is not hammered, and an FNV-1a OpenAI-key fingerprint that invalidates the entry on key rotation without storing the key.
- Codex: optional short micro-cache only; local read is cheap.
- Claude quota: never cached - always taken fresh from stdin.

### 5.4 Politeness invariant (must hold)

The statusline must NEVER call the Anthropic OAuth usage endpoint. Claude quota comes from stdin `rate_limits` only. Therefore the self-compose path calls `codex_local` + `openai_client` DIRECTLY and does not invoke the full `snapshot_composer::compose` (which fetches Claude OAuth and would 429 during active use - the exact pain v0.4.2 removes). This is why `statusline_render` does not depend on `anthropic_oauth`. The fresh-read path likewise takes only Codex + OpenAI from `snapshot.json`; Claude segments always come from stdin even when a snapshot is present.

## 6. Replace-any-statusline flow (PR4)

Today `setup` and the desktop Settings UI bail when they see a foreign `statusLine.command` (`WireStatus::OccupiedBy(cmd)`). New, provider-agnostic behavior:

- `claude_statusline::wiring` gains `replace_statusline(path, invocation)` - backs up the prior `command` (stored in Balanze's `settings.json`, e.g. `statusline.replaced_command`) and writes Balanze's invocation - and `restore_statusline(path)` - restores the backed-up command (or unwires if none).
- `setup`, on `OccupiedBy(cmd)`: prompt "Replace `{cmd}` with Balanze's statusline? Your `{cmd}` config is left intact; restore anytime with `balanze-cli statusline restore`. [y/N]".
- Desktop Settings UI: the same replace-with-consent plus a Restore control, extending the existing wire/unwire IPC commands.
- Invariant: the foreign tool's own config files (for example `cship.toml`) are never read, modified, or deleted. Only Claude Code's `settings.json` `statusLine` stanza is touched, and the prior value is always recoverable.

A `balanze-cli statusline restore` entry point exists (Claude Code only ever invokes `statusline` with no args + stdin, so an explicit `restore` arg is safe against the frozen invocation contract).

## 7. IPC / architecture changes to document

- New crate `statusline_render` in the `ARCHITECTURE.md` crate map.
- New IPC artifact `snapshot.json` (full `Snapshot` + `captured_at` + `schema_version`), writer = watcher/desktop host, reader = statusline. Documented in the `ARCHITECTURE.md` IPC contract alongside the existing `statusline.snapshot.json` (note the two files flow in opposite directions).
- Desktop Settings UI gains replace/restore on the existing statusline IPC commands (no brand-new command surface beyond restore).
- `settings.json` gains the `statusline` section (additive).

All of `README.md`, `AGENTS.md`, `docs/ARCHITECTURE.md`, `docs/PRD.md` are updated in lockstep (PR5), per the change-control rule.

## 8. Explicit non-goals / boundaries kept

- No unification of the three existing 50/90 color-bucket replications (`present.rs`, `tauri_sink.rs`, `quota.ts`). The statusline brings its own config-driven per-segment thresholds; touching the others is an out-of-scope drive-by refactor.
- The `statusline` subcommand's stdin/no-arg invocation contract stays frozen; only an additive `restore` arg and a `--codex-preset` print flag are added.
- The existing `statusline.snapshot.json` (statusline -> watcher) keeps its current shape and direction.
- No Anthropic OAuth call from the statusline, ever (5.4).
- No auto-write of Codex config; preset is print-only.
- Subscription leverage is never default-on in the line (D7).

## 9. PR slicing (5 stacked PRs)

Each PR is independently shippable, has a review checkpoint, and respects 3.1 at its boundary.

1. PR1 - Segment engine + config + colored Claude-only line. New `statusline_render` crate; `StatuslineConfig` in `settings.json`; `claude_statusline` parser extension (model/agent/context/effort); config-driven coloring + the style->ANSI parser; reproduce and color today's Claude segments (model, agent, context_bar, cost, 5h/7d with reset countdown + pace + staleness-for-Claude-where-applicable). No new network, no new IPC. Test-first pure renderer.
2. PR2 - Cross-provider via watcher snapshot (Hybrid read path). Watcher/desktop write `snapshot.json`; statusline renders Codex % + OpenAI $ from it when fresh; graceful Claude-only fallback when absent/stale. The IPC addition. Zero statusline-initiated network.
3. PR3 - Self-compose fallback + machine-wide OpenAI cache. When no fresh snapshot, compose Codex (local) + OpenAI (cached API) directly (not via the OAuth-touching composer); the 300s TTL cache with stale-while-updating, 300s negative cooldown (= the 3.1 gate), and key fingerprint. The only PR that adds statusline-initiated network, landing with its politeness gate.
4. PR4 - Replace-any-statusline flow. `wiring` replace/restore with backup; `setup` + desktop Settings UI replace-with-consent + restore; provider-agnostic; foreign config files untouched.
5. PR5 - Codex preset + docs + release. `--codex-preset` print helper; README / ARCHITECTURE / TROUBLESHOOTING / PRD / CHANGELOG updates; version bump to 0.4.2; file the deferred dev-context-line GitHub issue (D4).

## 10. Testing strategy (per AGENTS.md 6-7)

- `statusline_render` pure renderer: unit tests, test-first - snapshot + config -> exact string; threshold coloring at base/warn/critical; layout from `lines`; segment-absent rendering (no agent, no Codex, etc.).
- Style->ANSI parser: unit tests for fg/bg hex + bold/italic/underline + invalid input.
- `claude_statusline` parser extension: extend unit tests + the `real-payload.json` fixture for the new fields; real-data smoke against live Claude Code.
- Pace + reset countdown: unit tests with a fixed `now` and known `resets_at` (including a just-reset / out-of-range window degrading cleanly).
- Cache (PR3): TTL/mtime, stale-while-updating, negative cooldown, key-fingerprint invalidation - with an injected clock and a `BALANZE_*_OVERRIDE`-style cache dir; assert the OpenAI fetch is gated to at most once per 300s.
- Hybrid selection: snapshot fresh vs stale vs absent -> correct source path; Claude segments always from stdin even when a snapshot is present.
- Replace flow (PR4): `wiring` replace/restore round-trip over a temp `settings.json`; OccupiedBy -> replace -> restore returns the exact prior command; foreign config files are never opened.
- Integration: extend the `balanze_cli` end-to-end test with statusline rendering over committed fixtures + a fixed `now`.
- Manual (per PR, listed at each PR boundary): `bun run tauri dev` smoke for the Settings UI replace/restore (PR4); a real switch from cship and back on the dev machine; confirm no OAuth 429 during an active Claude session once Balanze owns the slot.

## 11. Risks

- Stdin field names for model/context/effort/agent may differ from cship's assumptions across Claude Code versions - mitigated by verifying against a live payload in PR1 and the existing drift-tolerant parse (unknown fields ignored, incomplete blocks dropped with a warning).
- `snapshot.json` freshness TTL tuning: too tight wastes the fresh-read path, too loose shows stale cross-provider numbers - mitigated by the staleness marker (D6) so a stale value is labeled, not hidden.
- Width pressure on the one-liner with all default-on segments - mitigated by the configurable `lines` and per-segment toggles.

## 12. Deferred (tracked, not built here)

- Reproduce the dev-context (Starship) line: GitHub issue to investigate shell-out to `starship prompt` vs a native minimal dir+git line (D4).
- v0.7: surface these segment/color choices in the Settings panel UI (not just the config file).
- Light/dark auto-detection of terminal background.
- True per-day OpenAI buckets and history-query segments (ride the post-1.0 dashboard's durable storage).
