# Balanze

## Overview

Balanze is a desktop utility for tracking personal AI usage across multiple providers in one place. The initial target is a tray-first desktop app built with Tauri 2 and Rust for Windows 11 and macOS 15+, with Ubuntu 24.04 LTS+ on GNOME added in a later phase.

The goal is simple: reduce the friction of checking multiple tools, tabs, billing pages and account surfaces to answer a few recurring questions - How much of Claude has been used today? How much OpenAI API credit remains? When does a limit reset? How close is the current account to budget or cap?

This is a side project, so the product should optimize for usefulness, low maintenance, and tight scope.

## Project intent

- **Quality over reach.** Effort goes into a clean, honest, well-tested core and a credible UI rather than maximizing user count. Distribution stays deliberately lightweight (a runnable release, not an app-store presence).
- **Narrow but deep.** Two providers done honestly beats six done shabbily. Broad provider coverage is a "someday," not a near-term goal.
- **Honesty is a first-class feature.** Every metric carries its source and confidence, and the UI makes that visible. The data-provenance model is treated as part of the product surface, not a footnote - see the matrix rules below.
- **Bounded, shippable increments.** The roadmap evolves in small phases that each stand on their own, rather than one big release.

## Problem

Heavy AI users increasingly split work across consumer subscriptions and API accounts. A single person may use ChatGPT for interactive work, Claude for coding or long-form reasoning, and one or more API accounts for automation, while each platform exposes usage, billing, credits and limits in different ways.

Today, the available tooling is fragmented. Existing open-source tools are often Claude-specific, CLI-only, IDE-bound, or focused on developer observability rather than personal cross-provider usage monitoring. This creates three user problems:

- Time wasted checking multiple sources manually.
- Poor awareness of reset windows, credit depletion, and spend trends.
- No single normalized view across subscription usage and API usage.

## Users

### Primary user

An individual power user, developer, founder, product manager, or researcher who actively uses more than one AI platform and wants lightweight visibility without running a SaaS admin product.

### Secondary user (future)

A small team or technically inclined individual who wants a local dashboard and alerting layer for personal or shared accounts, but not full enterprise observability.

## Goals

### Product goals

- Provide one app that shows AI usage across multiple providers in a normalized way.
- Support both subscription-style usage and API billing or credit usage where feasible.
- Make the app useful from the tray or menu bar with minimal interaction.
- Provide a consistent UX across operating systems while following each OS's design conventions.
- Keep all core functionality local-first and transparent.
- Preserve a clean path to later add Android and a hosted dashboard without rewriting the core architecture.
- Be glanceable, simple, colorful.

### Non-goals

- Full enterprise cost allocation or multi-seat observability.
- Native OS widgets in the initial roadmap.
- Broad Linux desktop support beyond Ubuntu GNOME.
- Browser automation or brittle scraping as a headline feature.
- Monetization, subscriptions, cloud sync, or team billing in the first version.
- Heavyweight distribution (code-signing, notarization, app-store/package-manager presence, auto-update) as a committed goal - distribution stays lightweight (a runnable release); the signing/store work is optional, not a phase the roadmap depends on.
- Broad provider coverage - the product stays narrow-but-deep; additional connectors are Vision-tier.

## Product principles

- Honest about data quality and source provenance.
- **Show measured status, not forecasts.** Present what *is* - how much is used and how far through the window you are - and let the user judge what comes next. Applies to money (the measured-only matrix) and to time.
- Fast glanceability before deep dashboards.
- One shared core with thin platform shells.
- Narrow support matrix over broad but unreliable compatibility.

## Scope

### In scope for MVP

- Desktop application using Tauri 2 + Rust.
- Tray or menu-bar presence with popup and optional full dashboard window.
- Provider connectors for OpenAI and Anthropic.
- Unified account list across subscription and API account types.
- Manual refresh plus periodic background refresh.
- Local secure storage for credentials and preferences.
- Simple historical trend storage for recent usage.
- Alerts for thresholds and reset windows.

### Out of scope for MVP

- Android app.
- Hosted dashboard.
- Collaboration features.
- Team analytics and seat breakdowns.
- Provider marketplace with dozens of integrations.
- Native widgets.

## Supported platforms

| Platform | Support level | Notes |
|---|---|---|
| Windows 11 | CLI v0.1; tray UI v0.3 | CLI works from v0.1 (`cargo install`); tray-first desktop experience lands across the v0.3 sub-milestones (popover in v0.3.0). Windows 10 excluded. |
| macOS 15+ | CLI v0.1; tray UI v0.3 | CLI works from v0.1; menu-bar-first experience with hidden main window across v0.3. |
| Linux (generic) | CLI v0.1 | `cargo install` works trivially; no separate test matrix. CLI only - tray UI not targeted here. |
| Ubuntu 24.04 LTS+ GNOME | Vision (uncommitted) | GNOME tray UI with AppIndicator support. Deferred until the Win + Mac tray story is mature; Linux tray fragility makes it the wrong place to start. |
| Android | Vision (uncommitted) | Companion app only after desktop proves value. Read-only feed of the desktop state via the sync layer. |
| Hosted web dashboard | Vision (uncommitted) | Separate surface for wallboard or TV use, reusing normalized backend contracts. |

## Key use cases

- See all major AI accounts and balances in one glance.
- Check Claude subscription usage and reset timing without opening the web app, where reliable data is available.
- Check the currently consumed subscription usage vs. the currently elapsed time in the time window.
- Check OpenAI API spend and remaining credits quickly.
- Get notified when nearing a usage limit or spending threshold.
- Compare current billing-cycle usage across providers.

## Core user experience

### Main modes

The app should support three presentation modes from the same codebase:

- Tray or menu-bar only.
- Tray or menu-bar with compact popup.
- Full dashboard window, optionally hidden until opened.

Users should be able to configure startup behavior, refresh cadence, alert thresholds and preferred compact metrics.

### Information hierarchy

The compact view should prioritize:

1. Provider status.
2. Current usage or spend.
3. Remaining credits or estimated remaining quota.
4. Reset time or billing-cycle progress.
5. Alerts.

The full dashboard should add:

- Per-provider detail pages.
- Recent trends.
- Account source and confidence indicators.
- Settings, connectors and troubleshooting.

## Functional requirements

### Provider model

The system should normalize data into a common schema:

- Provider name.
- Account type: subscription, API, team, or other.
- Metrics: spend, credits remaining, usage consumed, reset time, billing-cycle window, request or token counts where available.
- Data source type: official API, official dashboard, imported file, manual input, inferred.

### The usage matrix (presentation contract)

The headline view is a 2×2 matrix - Anthropic / OpenAI × subscription **quota %** / real billed **$** - and it follows one rule that the whole honesty story rests on: **the matrix holds measured reality only.**

A cell is filled only by a number that was actually measured or actually billed:

- **Anthropic quota %** - server-authoritative OAuth utilization (real).
- **OpenAI quota %** - Codex CLI rate-limit % (real).
- **OpenAI billed $** - Admin Costs API (real).
- **Anthropic billed $** - the `extra_usage` pay-as-you-go overage, *only* if the user enabled it on claude.ai (real, exact cents). Otherwise the cell reads as honestly **unavailable** ("Anthropic exposes no per-user API spend") - it is never backfilled with a substitute number. Recovering real Anthropic API spend/balance stays an open future item.

**Estimates and counterfactuals never occupy a matrix cell.** The list-price figure derived from local JSONL - what the same Claude Code usage *would* cost at API list prices - is presented separately as a **"Subscription leverage"** insight: explicitly a value/counterfactual stat ("leverage you got from the subscription"), never billed, visually and structurally distinct from the grid.

Rationale: a grid asserts that cells in the same column are the same kind of number. Putting a counterfactual list-price estimate in the same column as real billed spend claims a comparability that does not exist - which reads as misleading even when the text is honest. Keeping the matrix measured-only makes the honesty **structural** rather than dependent on labels. This contract governs both the CLI compact view and the v0.3 popover; the `--json` schema is unaffected (every money cell already carries `source`/`confidence`, so machine consumers disambiguate from the wire shape).

### Integrations

#### OpenAI

The product should support API usage and billing-related visibility using official surfaces where available, including usage and dashboard concepts already documented by OpenAI.

#### Anthropic

The product should support Claude-related usage views across API billing and subscription usage where reliable access is available. Claude documents subscription usage and length limits as dynamic and plan-dependent, so the app should present these carefully and avoid overstating precision.

### Alerts

Users should be able to define alerts for:

- Spend exceeds threshold.
- Credits remaining below threshold.
- Subscription usage nearing cap.
- Reset window approaching.
- Connector failure or stale data.

### Storage

- Store credentials locally using OS-appropriate secure storage when possible.
- Store settings and recent history locally.
- No mandatory cloud backend.

## UX requirements

- The compact popup should open fast and be glanceable in under 5 seconds.
- The tray or menu-bar state should surface the single most useful live indicator, such as total spend today, provider warning count, or next reset.
- Visual design should remain functional and lightweight rather than dashboard-heavy.
- Users should understand at a glance what is official data versus inferred data.

## Technical approach

### Architecture

Recommended architecture:

- Rust core for data normalization, polling, storage, alerting, and provider connector logic.
- Tauri desktop shell for tray behavior, windows, OS integration and packaging.
- Shared frontend for popup and dashboard views.
- Connector abstraction layer per provider so future Android or hosted surfaces can reuse the same normalized model.

### Design implications

This architecture keeps the desktop product as the first-class surface while making future web or Android clients consumers of the same normalized domain model, rather than forks of the business logic.

## Risks and constraints

### Main risks

- Provider data access may be inconsistent across API usage versus subscription usage.
- Subscription tracking may depend on surfaces that are not designed as public integrations.
- Linux tray behavior remains more fragile than Windows or macOS even with a constrained GNOME-only support policy.
- Side-project time constraints increase the importance of strict scope control.

### Mitigations

- Launch with only a small set of high-value integrations.
- Mark all metrics with source provenance and confidence.
- Prefer official endpoints and documented surfaces first.
- Treat unsupported subscription data as optional, not foundational.
- Keep the platform matrix narrow and explicit.

## Success criteria

Usefulness:

- The app becomes the default place to check AI usage daily.
- Two providers work reliably enough for personal use.
- The tray or compact popup answers the core status question in a few seconds.
- Setup time for one provider is under 10 minutes.
- Alerting reduces surprise credit depletion or quota exhaustion (v0.3.2 onward, configured in the settings UI; v0.1–v0.3.1 ship without alerts).

Engineering bar (what "finished-feeling" means here):

- The tray UI reads as intentional and credible, not a scaffold - clean enough that a screenshot stands on its own.
- The source/confidence model is legible at a glance: an onlooker can tell real billed spend from a quota % from a counterfactual estimate without reading documentation.
- The codebase tells its own story: the architecture, the data-provenance model, and the "measured status, not forecasts" decision (why the EWMA predictor was built, dogfooded, and then retired in favour of honest pace facts) are explained well enough (README + a short "how it works" writeup) that someone can understand the interesting decisions without spelunking.
- Someone can download a release and run it without a Rust toolchain (lightweight distribution, v0.4).

## Phasing

The MVP lands across four release phases plus an uncommitted Vision tier. Each phase has **one dominant theme** - Data → Liveness → UI → Distribution & Legibility → Vision - so "done" for each is hard to fudge and risk is sequenced correctly (read-only data primitives first, asymmetric/UI work later). v0.3 (UI) is delivered as **bounded sub-milestones** (v0.3.0-v0.3.3) that each ship on their own, so the hero artifact - the popover - exists early rather than at the end of one large release. The detailed build sequence lives in the design doc; this is the product-level summary.

### Phase 1 - v0.1: Data (shipped: `v0.1.0` / `v0.1.1`)

A complete, honest **data layer** exposed as a CLI (`balanze-cli`). No tray UI - the CLI prints the same normalized snapshot the popover later shows. The bar was the **four-quadrant matrix fully lit**:

| | Quota % | API $ (real billed) |
|---|---|---|
| **Anthropic** | OAuth usage endpoint (5h / 7-day / per-model cadence bars + reset clocks) | `extra_usage` overage if the user enabled pay-as-you-go (real); else **unavailable**. The JSONL list-price estimate is the separate "Subscription leverage" insight, not this cell |
| **OpenAI** | Codex CLI rate-limit % (local `~/.codex/sessions/` rollout files) | real billed spend (Admin Costs API) |

Per the matrix presentation contract above, every cell holds measured reality only.

- Claude subscription utilization + reset clock via Anthropic's OAuth usage endpoint (`GET api.anthropic.com/api/oauth/usage`, Bearer from `~/.claude/.credentials.json`, searched in both `~/.claude/` and `~/.config/claude/`). Authoritative; no scraping.
- Per-event detail (per-model breakdown, burn rate, rolling window) via local JSONL parsing of `<claude_home>/projects/**/*.jsonl` - no API, no auth. Events deduped by `(message_id, request_id)`.
- **The Anthropic billed-$ cell shows real money or nothing.** Anthropic exposes no per-user API spend (the official Usage & Cost API is org-admin-gated, NO-GO for the modal user). The cell holds the real `extra_usage` pay-as-you-go overage when enabled, otherwise reads **unavailable** - never backfilled. The `claude_cost` list-price figure (local JSONL × a vendored LiteLLM price table) is the separate **"Subscription leverage"** insight - what the same usage would cost at API list prices, never billed.
- OpenAI Codex quota % from the local Codex rollout files (`~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-*.jsonl`, server-computed `rate_limits.primary`).
- OpenAI API spend via the documented Admin Costs API (`GET /v1/organization/costs`, `sk-admin-…` Bearer).
- `balanze-cli setup` - interactive wizard: checks Anthropic OAuth, checks Codex sessions, prompts + live-validates the OpenAI admin key (masked), stores it in the OS keychain.
- `--sections` (per-source detail) and `--json` (machine-readable snapshot) output modes - flags on `status`, also accepted as bare top-level shortcuts.
- Local secure storage (keychain for secrets; `directories`-crate per-OS paths for non-secret settings). Known Windows keychain limitation documented, with a `BALANZE_OPENAI_KEY` env-var fallback.
- **Distribution: source only.** `cargo install --git https://github.com/Oszkar/balanze balanze_cli`. No binaries, installers, or GitHub Releases in v0.1; the audience accepts the Rust-toolchain prerequisite. Linux works via `cargo install`.

### Phase 2 - v0.2: Liveness (shipped: tagged `v0.2.0`)

The data updates itself and the Anthropic API $ figure is made honest. Still CLI-only.

- **Live spine = statusline-push + JSONL `notify`.** Claude Code's `statusLine` command receives, zero-auth and push-driven (per turn, debounced), `rate_limits.{five_hour,seven_day}.{used_percentage,resets_at}` - the same server-authoritative window data `/api/oauth/usage` returns - plus `cost.total_cost_usd`. Because OAuth `/api/oauth/usage` 429s the account *exactly during active Claude Code use*, statusline is the primary live signal; OAuth is demoted to a slow backoff'd cold/fallback poll (`backoff::standard()`, 30s×2ⁿ, 429-tolerant, stale-with-warning) and local JSONL stays the always-available activity/estimate source. The `claude_statusline` crate owns this evolving payload schema with `claude_parser`-grade drift discipline.
- **Anthropic API $ honesty redesign.** The premise that Claude Code records a per-event cost in the JSONL is **false for current Claude Code** (verified absent across 790 session files; Anthropic removed `costUSD`). So the JSONL × list-price figure is hard-labeled "estimate / subscription leverage / NOT billed", and the real **`extra_usage` pay-as-you-go overage** is surfaced as a distinct real-money line (cents, exact, reconciled 3/3 against claude.ai). Three explicitly-labeled cost tiers now coexist and never conflate: JSONL list-price estimate, statusline session-cost estimate, real `extra_usage` overage. The current prepaid *balance* still has no per-user API; the Console cookie-paste that could supply it is demoted to "implement only if a real user need surfaces".
- **`balanze-cli --watch`** long-running loop (Stdout + JSONL sinks under a `tokio::select!` supervisor) + the `statusline` output mode for shell prompts; `setup` wires the `statusLine` config for the user.
- **Pace replaces the EWMA predictor.** The predictor (EWMA + Insufficient→Uncertain→Confident warm-up machine) was built, dogfooded, and retired: forecasting is the wrong mental model for a trust-first tool. The **pace model** - measured *quota used %* vs *window elapsed %* plus a transparent used÷elapsed ratio, no forward forecast - folds into `window` per the "measured status, not forecasts" principle.
- Shared `snapshot_composer` (CLI ≡ watcher, fixture-parity-tested), `backoff` exponential-retry on both HTTP clients, Criterion baselines for the cost/parse hot paths, and the live `TauriSink` seam validated as a compile-only skeleton ahead of v0.3.

### Phase 3 - v0.3: UI

The Tauri surface - the hero artifact. The full UI scope (popover, settings, alerts, dashboard) ships as **bounded sub-milestones**, each shippable on its own, so the popover screenshot exists early. The biggest known risk - the `state_coordinator` `Sink` / `TauriSink` seam - is exercised live in the very first sub-milestone.

**v0.3.0 - Popover (the hero). Shipped.** The glanceable surface, and the thing that makes the whole backend legible.

- Tauri 2 popover/tray UI: color-shifting gauge tray icon (RGBA ring rendered at runtime, repaint deduped by `(ColorBucket, title_text)`), hidden-on-launch popover (left-click toggles, blur hides) with one progress bar per Anthropic cadence + reset sublines, the burn number, the **pace view**, and the matrix tiles - in a transposed grid (providers as columns) plus a Cards density view.
- **Pace view (replaces the retired predictor).** Per window (5h / 7-day), two measured facts side by side - *quota used %* and *window elapsed %* - plus a transparent **pace ratio** (used ÷ elapsed) rendered as a glanceable verdict ("on pace" / "burning ~2.0× faster than linear"). Pure division of two measured numbers, **not** a forecast: always defined, no warm-up, no post-reset lie. The 30-minute burn rate shows alongside as a number (the sparkline glyph + series ride with durable history in v0.3.3).
- The tiles obey the **matrix presentation contract** (see Functional requirements): the 2×2 holds **measured reality only** - server quota % and real billed $ - each cell carrying a **visible source/confidence badge**, the primary quota source being the statusline feed with OAuth shown as the stale/fallback state. The **"Subscription leverage"** estimate renders as a separate, clearly-secondary insight outside the grid. Making the provenance model *visible* is a first-class goal of this milestone.
- Wires the live spine into the Tauri host (watcher → coordinator → `TauriSink`) and the popover IPC: commands `get_snapshot` + `refresh_now`, events `usage_updated` + `degraded_state`. `tauri-plugin-single-instance` prevents double-launch. (`get_history` stays in the contract but defers to v0.3.3 with the sparkline.)

**v0.3.1 - Settings & trust.**

- Settings UI: paste API keys, save to OS keychain. With a real key-input box the keychain code is exercised on both platforms, so this is where the **`keyring` → `keyring-core` (v4) migration** lands, fixing the v0.1 Windows keychain no-op. Adds `set_api_key` / `get_settings` / `set_settings`.
- Surfaces the `statusLine` wiring (the CLI `setup` does it headless; the settings UI shows/edits it).
- Degraded-state events surfaced visually (`degraded_state`): stale data shown with a warning rather than blanked.
- **Codex-staleness honesty.** When the latest Codex rollout has outlived the window it describes (`now > primary.resets_at`), degrade the indicator from `✓` to a stale marker instead of a confidently-wrong used % behind a green check. The same pass fixes the window-duration label - the 5-hour Codex primary window renders `0d` because `window_duration_minutes / 1440` floors to zero; show `5h`. Both in `balanze_cli::compact_codex_quota` and the popover's Codex cell.
- **Uniform serde-error redaction.** Route the serde `Display` through `redact_for_display` / `e.classify()` at the three top-level JSON-parse error sites (`anthropic_oauth` `refresh.rs` + `client.rs`, `openai_client` `client.rs`) so a type-confused provider 200 carrying an `sk-…`-shaped value can't leak into an error string, extending the precedent already applied to the nested `extra_usage` parse.

**v0.3.2 - Alerts.** Kept deliberately minimal - table-stakes, not gold-plated.

- OS notifications for: spend exceeds threshold, credits/quota below threshold, subscription approaching cap, reset window approaching, connector failure / stale data. Thresholds informed by v0.1–v0.2 observation, configured in the settings UI.

**v0.3.3 - Dashboard.**

- Optional full dashboard window: per-provider detail, recent trends/sparklines, source/confidence detail, troubleshooting.
- **Pulls in history persistence (SQLite).** Through v0.2 history is in-memory, rolling-window-sized. A trends view needs durable history, so the persistence layer (deferred since v0.1) lands here, as a dependency of the dashboard rather than speculative infrastructure.

**Demoted / not committed in v0.3:** the Anthropic Console cookie-paste. Its only unique signal is the *current* prepaid balance; `extra_usage` already gives the real overage spent/limit and statusline gives authoritative quota - so the fragile/unofficial scrape (§3.3) is implemented only if a concrete user need surfaces. If ever built: cookie-paste UX, tile shows `auth_expired` and prompts a re-paste on 401.

### Phase 4 - v0.4: Distribution & Legibility

Make it runnable without a Rust toolchain, and make the engineering legible to anyone who looks. Deliberately **lightweight** (see Project intent) - the heavyweight signing/store work is optional, not load-bearing.

- **Runnable release.** Unsigned binaries on GitHub Releases (MSI/NSIS, DMG/app) so someone can download and run it without `cargo`. Linux still via `cargo install`.
- **Legibility.** A polished README with screenshots / a short GIF of the popover, and a "how it works" writeup centered on the three things worth showing: the data-provenance model (the measured-only matrix + the leverage insight), the "measured status, not forecasts" call (built an EWMA predictor, dogfooded it, retired it for honest pace facts), and the actor-model architecture / the twelve boundaries.
- "Send Logs" menu item bundling rotated logs + a recent state snapshot for support.
- **Price-table refresh script.** A `scripts/refresh-claude-prices.*` mechanizing the vendored LiteLLM Anthropic price-table refresh (fetch → filter to `claude-*` → save with a `_meta` block → bump the `include_str!` path → `cargo test -p claude_cost`). Low-priority: the list-price recompute is a diagnostic fallback, but that fallback still needs a current table. The procedure is documented manually in `crates/claude_cost/README.md` today; mechanizing it removes a footgun.
- **Optional (not committed):** Windows code-signing, macOS notarization, Homebrew tap, WinGet manifest, Tauri auto-update. Done only if the cert/admin cost feels worth it - low engineering-taste signal per hour, and unsigned-runnable already clears the "an evaluator can try it" bar. (If pursued, the release pipeline itself - notarization, auto-update manifest - is the artifact worth showing.)

### Vision - uncommitted

Genuinely uncommitted ideas, picked up as a bounded phase only if desire or a real need surfaces. Not promised, not sequenced - listed so the architecture keeps them cheap.

- **Prove the connector abstraction** - add a third provider (Gemini CLI / OpenRouter / Cursor) to turn the "one shared core, thin connectors" claim from asserted into demonstrated. The strongest single "this architecture generalizes" signal, if/when it's wanted.
- Ubuntu 24.04 LTS+ GNOME support - deferred until the Windows + macOS experience is solid (GNOME tray behavior is the most fragile of the three).
- Cross-device sync via a small relay (GitHub Gist / Cloudflare KV / iCloud Drive) - one Balanze identity reading/writing the same numbers across devices; sets up Android + hosted dashboard cleanly.
- Export / snapshot reporting; broader provider coverage as repeated personal need surfaces.
- Android companion app - read-only feed of the desktop's state via the sync layer.

## Open questions

Items still genuinely unresolved at the product level. The design doc carries the technical open questions and spike plan.

- Which provider metrics are realistically obtainable through stable official integration versus inference? Resolved on the Anthropic side at the availability level: the official Usage & Cost API is enterprise/admin-gated (NO-GO for the modal user); current Claude Code JSONL carries no per-event cost (verified across 790 files), so the JSONL figure is labeled leverage-not-spend; the OAuth `extra_usage` block is the claude.ai pay-as-you-go overage meter (cents, exact, real billed money) and is surfaced as such; and statusline carries authoritative 5h/7d %+resets plus a client-side session-cost estimate (the v0.2 live backbone). The Anthropic Console cookie-paste (current prepaid balance) is demoted to "implement only if a real user need surfaces". Broader provider coverage in later phases needs similar per-provider investigation.
- Should onboarding ask users to choose a trust mode, such as "official only" versus "include estimated subscription metrics"? Deferred - v0.1 marks every metric with its source per the Transparency requirement, which may make an explicit trust-mode setting unnecessary.
- How much locally-stored history is needed before the product becomes meaningfully better than a simple current-status tool? v0.1–v0.2 keep a rolling-window-sized in-memory history. SQLite persistence is now scoped to **v0.3.3**, as a dependency of the dashboard's trends view (rather than speculative infrastructure) - the dashboard is the first surface that genuinely needs durable history.
- What is the right default alert threshold mix (alerts land in **v0.3.2**)? Decide after observing real-use patterns through v0.1–v0.3.1 - premature thresholds are noise.
- How should the popover present the matrix and the leverage insight visually? Resolved by the matrix presentation contract (measured-only grid + separate "Subscription leverage" insight) and the shipped v0.3.0 popover (visible source/confidence badges); retained here as the rationale for that treatment.
