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

- Provide one app that shows AI usage across multiple providers in a normalized way - both subscription-style usage and API billing or credit usage, where feasible.
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
- Heavyweight distribution (app-store/package-manager presence, auto-update) as a committed goal - distribution stays lightweight (a runnable release). macOS code-signing/notarization is done (issue #160, Keychain "Always Allow" grants need a stable signature to persist); Windows code-signing is declined on the merits - see "Code signing" under [Supported platforms](#code-signing).
- Broad provider coverage - the product stays narrow-but-deep; additional connectors are Vision-tier.

## Product principles

- **Show measured status, not forecasts.** Present what *is* - how much is used and how far through the window you are - and mark every metric with its source and confidence; let the user judge what comes next. Applies to money (the measured-only matrix) and to time.
- Fast glanceability before deep dashboards.
- One shared core with thin platform shells.

## Scope

### In scope

- Desktop application using Tauri 2 + Rust.
- Tray or menu-bar presence with a compact popup.
- A mature CLI surface (status, watch live-TUI, doctor, export, completions/man, exit-code taxonomy) - also the Linux story.
- A productized, cross-provider Claude Code statusline (v0.4.2).
- Provider connectors for OpenAI and Anthropic.
- Unified account list across subscription and API account types.
- Manual refresh plus periodic background refresh.
- Local secure storage for credentials and preferences. (Durable local history is deferred to the post-1.0 dashboard; the CLI `export` re-derives history from JSONL statelessly.)
- Runnable distribution: downloadable binaries, no Rust toolchain required (v0.5).

### Out of scope

- Android app.
- Hosted dashboard.
- Collaboration features.
- Team analytics and seat breakdowns.
- Provider marketplace with dozens of integrations.
- Native widgets.

## Supported platforms

| Platform | Architecture | Support level | Notes |
|---|---|---|---|
| Windows 11 | x64 | CLI v0.1; tray UI v0.3 | CLI works from v0.1 (`cargo install`); tray-first desktop experience lands across the v0.3 sub-milestones (popover in v0.3.0). Windows 10 excluded. Installers are unsigned by decision - see "Code signing" below. arm64 Windows not built. |
| macOS 15+ | Apple Silicon (arm64) | CLI v0.1; tray UI v0.3 | CLI works from v0.1; menu-bar-first experience with hidden main window across v0.3. Release DMG/app signed and notarized from v0.5.0. |
| macOS, Intel | x86_64 | **Excluded** | No release artifact is built. macOS 15 already drops most Intel hardware, so a universal binary would double mac build time and bundle size to serve a shrinking set of machines that largely cannot run the documented OS floor. `cargo install` from source is untested but unobstructed. |
| Linux (generic) | x64 | CLI v0.1 | `cargo install` works trivially; no separate test matrix. CLI only - tray UI not targeted here. |
| Ubuntu 24.04 LTS+ GNOME | x64 | Future | GNOME tray UI with AppIndicator support. Deferred until the Win + Mac tray story is mature; Linux tray fragility makes it the wrong place to start. |

The architecture column is load-bearing: it must track `release.yml`'s build matrix and `deny.toml`'s `[graph].targets` exactly. Adding an architecture means touching all three.

### Code signing

**macOS: signed and notarized** (v0.5.0 onward, issue #160). Gatekeeper does not warn on the release DMG.

**Windows: unsigned, by decision - not a backlog item.** This is deliberate, and the usual "buy a certificate" answer no longer buys what it used to:

- **No certificate at any price gives a clean first run.** Microsoft [documents](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation) that EV certificates no longer bypass SmartScreen ("Paying a premium for EV solely to avoid SmartScreen warnings is no longer justified"); DigiCert's KB confirms it. Reputation still accrues by download volume regardless of signing.
- **OV has no cost advantage left.** CA/B Forum ballot CSC-13 (effective June 2023) removed software-key storage for OV, so it needs the same hardware token or cloud HSM as EV.
- **Azure Artifact Signing (~$10/mo, the one option Tauri documents a CI recipe for) is geographically closed to us.** Individual developers are eligible in the USA and Canada only; the maintainer is a Japan-based sole proprietorship, which is neither an eligible individual nor an eligible registered organization for this program.

What signing would actually buy: a displayed publisher name, and reputation that carries across releases instead of resetting per version. Neither justifies the cost at this project's cadence and funding. The trust substitutes we ship instead: public source, reproducible build-from-source, and per-release SHA-256 checksums.

Revisit if the project ever incorporates in an eligible jurisdiction, or if [SignPath Foundation](https://signpath.org/) (free for OSS) becomes viable - its unquantified "verifiable reputation" gate is the current blocker there.

## Key use cases

- See all major AI accounts and balances in one glance.
- Check Claude subscription usage and reset timing without opening the web app, where reliable data is available.
- Check the currently consumed subscription usage vs. the currently elapsed time in the time window.
- Check OpenAI API spend and remaining credits quickly.
- Get notified when nearing a usage limit or spending threshold.
- Compare current billing-cycle usage across providers.

## Core user experience

Balanze has several surfaces: tray popover GUI, live TUI, CLI command and Claude Code statusline (footer). These should provide a unified user experience and matching information - as much as the surface allows.

### Information hierarchy

The current compact views should prioritize:

1. Provider status.
2. Current usage or spend.
3. Remaining credits or estimated remaining quota.
4. Reset time or billing-cycle progress.
5. Alerts.

The (future) full dashboard should add:

- Per-provider detail pages.
- Account source and confidence indicators.
- Settings, connectors and troubleshooting.
- Token usage and trends.
- Deeper cost analysis.

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

The product should support Claude-related usage views across API billing and subscription usage where reliable access is available. Claude Code owns its OAuth credential in every storage form; Balanze reads it without refreshing, modifying, mirroring, or backing it up, and directs the user to `claude login` when it expires. An explicit file-refresh opt-in may be considered during settings and configurability work, but must remain off by default and can never make the macOS Keychain source writable. Claude documents subscription usage and length limits as dynamic and plan-dependent, so the app should present these carefully and avoid overstating precision.

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
- A bounded actor owns the live snapshot. Settings changes are acknowledged only after the previous poller generation is joined and the replacement generation is active; stale-generation updates are rejected. Durable snapshot publication is coalesced on a dedicated blocking writer so local storage latency cannot freeze the tray or IPC queries.

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
- Alerting reduces surprise credit depletion or quota exhaustion (v0.6, configured in the settings UI; v0.1-v0.5 ship without alerts).

Engineering bar (what "finished-feeling" means here):

- The tray UI reads as intentional and credible, not a scaffold - clean enough that a screenshot stands on its own.
- The source/confidence model is legible at a glance: an onlooker can tell real billed spend from a quota % from a counterfactual estimate without reading documentation.
- The codebase tells its own story: the architecture, the data-provenance model, and the "measured status, not forecasts" decision (why the EWMA predictor was built, dogfooded, and then retired in favour of honest pace facts) are explained well enough (README + a short "how it works" writeup) that someone can understand the interesting decisions without spelunking.
- Someone can download a release and run it without a Rust toolchain (lightweight distribution, v0.5).

## Phasing

The MVP lands across five release phases - Data -> Liveness -> UI -> Surfaces -> Distribution - followed by two committed enhancement phases (Alerts, then Settings & configurability) and a post-1.0 deferred tier (Dashboard, then the uncommitted Vision ideas). Each phase has **one dominant theme** so "done" is hard to fudge and risk is sequenced correctly (read-only data primitives first, asymmetric/UI work later). Shipped phases are summarized to a line here; the blow-by-blow lives in the [CHANGELOG](../CHANGELOG.md), and this section stays forward-looking.

**Current status: shipped through v0.5.0 (Distribution & Legibility).** The data layer, the live spine, the Tauri popover + settings, the UI polish pass, the first-class CLI surface, the productized cross-provider statusline, first-class Codex treatment (both rolling windows, everywhere), a cross-cutting hardening pass, and the first signed/notarized binary release with launch-at-login are done, and the product is useful day to day and downloadable without a Rust toolchain. The remaining work covers CLI distribution (v0.5.1), the docs site and the how-it-works writeup (v0.5.2), and support tooling plus a hardening pass (v0.5.3); v0.6 adds alerts; v0.7 deepens settings and theming.

### Phase 1 - v0.1: Data (shipped)

A complete, honest **four-quadrant data layer** exposed as a CLI (`balanze-cli`): Anthropic quota % (OAuth) and the real `extra_usage` overage $, OpenAI Codex quota % and real billed $ (Admin Costs API), with the JSONL list-price figure kept separate as the "Subscription leverage" insight. Every cell holds measured reality only, per the matrix presentation contract (see Functional requirements). Distribution was source-only (`cargo install`). Full feature list in the CHANGELOG.

### Phase 2 - v0.2: Liveness (shipped)

The data updates itself and the Anthropic API $ story is made honest. Live spine = Claude Code `statusLine` push + JSONL `notify`, with OAuth demoted to a backoff'd fallback poll (it 429s exactly during active use). The **pace model** - measured quota-used % vs window-elapsed %, no forecast - replaced a built-then-retired EWMA predictor, the "measured status, not forecasts" call the trust story rests on. Still CLI-only.

### Phase 3 - v0.3: UI (shipped through v0.3.1)

The Tauri surface - the hero artifact - delivered as bounded sub-milestones so the popover screenshot existed early.

**v0.3.0 - Popover (shipped).** Color-shifting gauge tray icon + hidden-on-launch popover: one progress bar per Anthropic cadence, the pace view, the burn number, and the matrix tiles with visible source/confidence badges (transposed grid + a Cards density view). Wires the live spine into the Tauri host (watcher -> coordinator -> `TauriSink`) and the popover IPC (`get_snapshot`, `refresh_now`, `usage_updated`, `degraded_state`).

**v0.3.1 - Settings & trust (shipped).** Settings UI (keys to keychain, live provider toggles, statusLine wiring), the Windows `keyring-core` fix, the macOS Keychain OAuth read, Codex-staleness honesty, and the degraded-state banner.

**Phase 3 is done at v0.3.1.** The popover and settings are a credible UI. Alerts - originally sketched here as v0.3.2 - are resequenced into their own phase below, after distribution, so the product reaches real users sooner; the dashboard (sketched as v0.3.3) is deferred further still, past 1.0.

### Phase 4 - v0.4: Surfaces

Make every surface - popover, CLI, statusline - first-class and presentable. The theme is **"each way in reads as intentional, not a scaffold."** Four bounded sub-milestones, built in order.

**v0.4.0 - UI polish.** A lightweight visual pass, not a design system - the frontend is ~14 small components, so a token/component framework would be YAGNI. Tighten spacing, type scale, and color; grow the existing `theme.css` into a small token set; add real empty / loading / error states. The bar is the Success-criteria line: the UI reads as intentional, not a scaffold, and a screenshot stands on its own. Sequenced first because it feeds the v0.5 release screenshots.

**v0.4.1 - CLI polish** Turn `balanze-cli` from feature-complete into a first-class surface (it is also the entire Linux story). Surface note: `status` / `watch` are now explicit subcommands (was bare `--json` / `--sections` / `--watch`); bare `balanze-cli` still defaults to `status`. The CLI stayed glue - no new Tauri command/event (IPC contract unchanged), no change to the twelve architectural boundaries or the actor-model write boundary, and nothing persisted.

**v0.4.2 - Statusline polish.** Productize the Claude Code statusline (already shipped in v0.2 as the watcher's IPC bridge) into a first-class, installable display that **replaces** an existing statusline like cship - the low-friction way into Balanze for people who live in their coding-agent prompt. Ownership is exact: only the canonical `balanze-cli statusline` command is Balanze-owned, while wrappers and composed commands remain foreign. Concurrent prompt processes coordinate OpenAI fallback refreshes through the shared atomic cache rather than multiplying upstream requests.

**v0.4.3 - Codex maturity.** Codex stops being a second-class quota source. `codex_local` classifies each of Codex's two rolling windows (5-hour and weekly) by duration instead of trusting a plan-dependent JSON slot, and both windows surface everywhere - tray, popover, cards, CLI, and statusline - on the same shared 50/75/90 color scale as Claude. Alongside it: a parser-reliability fix (a single bad JSONL line no longer stalls the incremental cursor for the rest of the file), honest `BALANZE_LOG` file logging, and a quieter macOS Keychain path (no more re-prompting every poll tick).

**Phase 4 is done at v0.4.3.** Every surface reads as intentional, and Codex is now a peer citizen across all of them rather than an afterthought.

**v0.4.4 - Hardening (follow-on).** A cross-cutting reliability and correctness patch that settles the phase before Distribution rather than a fifth surface: Codex quota reads the account-wide limit instead of a per-model window, Claude credentials are strictly read-only on every platform, live settings transitions are supervised and race-free, and the weekly-window label and reset countdown read identically across every surface.

### Phase 5 - v0.5: Distribution & Legibility

Get it into people's hands and make the engineering legible. Deliberately **lightweight** (see Project intent) - signing/store work stays optional, not load-bearing. Like Phase 4, it ships as bounded sub-milestones under patch bumps rather than one tag.

**v0.5.0 - Runnable release & discoverability.** The first binary release: signed & notarized macOS DMG/app plus unsigned Windows MSI/NSIS on GitHub Releases, so someone can download and run it without `cargo` (the macOS signing path landed ahead of the tag). Alongside it: **launch-at-login** so the tray app behaves like a tray app after a reboot, a **README refresh** with a popover screenshot and the download install path, and a recorded **docs-site decision** (adopt mdBook; the site itself is built in v0.5.1). Done when the signed/notarized macOS release publishes successfully with assets attached.

**v0.5.1 - Reach.** CLI distribution: prebuilt `balanze-cli` binaries on Releases for Windows (x64 and arm64), macOS (Apple Silicon), and Linux (static musl), plus a Homebrew tap carrying a CLI formula and a desktop cask. Linux gets its first install path that is not `cargo install`. crates.io is **out by design**: the workspace is deliberately unpublishable (`publish = false`, and generic-named internal path crates like `window` / `keychain` / `settings`), so publishing the CLI would mean publishing its whole dependency graph into a shared namespace. Windows arm64 lands for the CLI only - the desktop app has a WebView2 dependency the CLI does not, so the desktop half stays open.

**v0.5.2 - Legibility.** The **"how it works" writeup** - the data-provenance model (the measured-only matrix + the leverage insight), the "measured status, not forecasts" call, and the actor-model architecture / the twelve boundaries - and the **mdBook docs site**: the skeleton, a GitHub Pages deploy, and a rendered home for the user guide, the writeup, and the surface-consistency design record. The generator decision was recorded in v0.5.0.

**v0.5.3 - Support & hardening.** A **price-table refresh script** (`scripts/refresh-claude-prices.*`) mechanizing the vendored LiteLLM Anthropic price-table refresh, a **"Send Logs"** menu item bundling rotated logs + a recent state snapshot for support, and the accumulated correctness debt: one Codex window-selection contract, a cross-process settings lock, provenance-safe statusline cache seeding, and the deferred pace-display call.

The v0.4.0 screenshots already exist, so the README refresh lands in v0.5.0 ahead of the rest.

**Optional (not committed):** Homebrew/WinGet, Tauri auto-update - done only if the admin cost feels worth it. Windows code-signing is no longer on this list; it is declined on the merits (see [Code signing](#code-signing)), and unsigned-runnable already clears the "an evaluator can try it" bar for Windows.

### Phase 6 - v0.6: Alerts

Kept deliberately minimal - table-stakes, not gold-plated. OS notifications for: spend exceeds threshold, credits/quota below threshold, subscription approaching cap, reset window approaching, connector failure / stale data. Configured in the settings UI with minimal inline thresholds, informed by accumulated real-use observation through v0.1-v0.5 - the deferral is deliberate, since premature defaults are noise. The richer settings / theming pass that deepens this config is v0.7.

### Phase 7 - v0.7: Settings & configurability

Deepen the configuration surface once there is enough to configure. The theme is **"make it yours."** The v0.3.1 settings UI already covers keys, provider toggles, and statusLine wiring; this phase grows it:

- **Color themes** - popover (and statusline) theming beyond the existing light/dark, so a screenshot can match a user's taste; the visible-polish win for a showcase.
- **Refresh cadence & startup** - user-set poll cadence (still floor-clamped per §3.1) and start-hidden behavior. (Launch-at-login shipped earlier, in v0.5.0.)
- **Statusline config in the UI** - surface the v0.4.2 segment / color choices in the settings panel, not just a config file.
- **Deeper alert UX** - the threshold / notification controls alerts (v0.6) shipped inline, refined into a proper per-alert configuration surface, now informed by real v0.6 usage.

The last committed pre-1.0 milestone; v1.0 follows.

### Post-1.0 - Dashboard & trends (deferred)

Past the v1.0 line - v1.0 is feature-complete at the end of v0.7 (surfaces + distribution + alerts + configurability). Kept here rather than in Vision because it is a committed-in-spirit consumer of work already done, just not pre-1.0.

The optional full dashboard window: per-provider detail, recent trends / sparklines, source/confidence detail, troubleshooting. Light: durable history was deferred from v0.4.1 (see Open Questions), so the dashboard is where persistence lands - the v0.4.x CLI `export` already re-derives Claude history from JSONL statelessly, so the history layer was decided before this (deferred, not invented here). Deferred past 1.0 because the popover + CLI TUI already answer the glanceable question, which makes the dashboard the least load-bearing surface and the right thing to cut from the critical path.

**Demoted / not committed:** the Anthropic Console cookie-paste. Its only unique signal is the *current* prepaid balance; `extra_usage` already gives the real overage and statusline gives authoritative quota, so the fragile/unofficial scrape (§3.3) is implemented only if a concrete user need surfaces.

### Vision - uncommitted

Genuinely uncommitted ideas, picked up as a bounded phase only if desire or a real need surfaces. Not promised, not sequenced - listed so the architecture keeps them cheap.

- **Prove the connector abstraction** - add a third provider (Gemini CLI / OpenRouter / Cursor) to turn the "one shared core, thin connectors" claim from asserted into demonstrated. The strongest single "this architecture generalizes" signal, if/when it's wanted.
- Ubuntu 24.04 LTS+ GNOME support - deferred until the Windows + macOS experience is solid (GNOME tray behavior is the most fragile of the three).
- Cross-device sync via a small relay (GitHub Gist / Cloudflare KV / iCloud Drive) - one Balanze identity reading/writing the same numbers across devices; sets up Android + hosted dashboard cleanly.
- Export / snapshot reporting beyond the v0.4.1 CLI surface; broader provider coverage as repeated personal need surfaces.
- Android companion app - read-only feed of the desktop's state via the sync layer.

## Open questions

Most of the original product-level questions are now resolved; they are kept here, marked, because the resolutions are load-bearing rationale.

- **Which provider metrics are realistically obtainable through stable official integration versus inference?** *Resolved for the two committed providers.* Anthropic's official Usage & Cost API is enterprise/admin-gated (NO-GO for the modal user); current Claude Code JSONL carries no per-event cost (verified across 790 files), so the JSONL figure is labeled leverage-not-spend; the OAuth `extra_usage` block is the claude.ai pay-as-you-go overage meter (cents, exact, real billed money); and statusline carries authoritative 5h/7d %+resets plus a client-side session-cost estimate. The Console cookie-paste (prepaid balance) is demoted to "implement only if a real user need surfaces". Broader provider coverage (Vision) needs similar per-provider investigation.
- **Should onboarding ask users to choose a trust mode ("official only" versus "include estimated metrics")?** *Resolved - won't build.* The shipped per-metric source/confidence badges plus the degraded-state banner make every metric's provenance visible at the point of use, which makes a global trust-mode toggle redundant.
- **How much locally-stored history is needed, and is durable storage (SQLite) worth adding now?** *Resolved - deferred.* The earlier answer was "SQLite lands in v0.4.1, earned by export"; the spike reopened it and v0.4.1 settled it: durable storage is **deferred with the post-1.0 dashboard**, not added now. Rationale: (1) Claude usage history is re-derivable from JSONL on demand (ccusage proves a fully stateless tool ships), so v0.4.1 `export` re-derives the full Claude `(day, model)` series from JSONL on every run; (2) OpenAI daily buckets *within* the current month are free from the Admin Costs API, so the only thing persistence buys is OpenAI history *across* months, which the post-1.0 dashboard is the first surface to actually need; (3) the time-series dashboard that most wanted a DB is itself post-1.0. So `export` was built stateless (no persistence), history-*query* commands fold in when the DB lands with the dashboard, and `get_history` stays planned-not-built.
- **What is the right default alert threshold mix?** *Deferred to v0.6.* Decided then, informed by accumulated real-use observation through v0.1-v0.5 - premature thresholds are noise.
- **How should the popover present the matrix and the leverage insight visually?** *Resolved.* The matrix presentation contract (measured-only grid + a separate "Subscription leverage" insight) plus the shipped v0.3.0 popover (visible source/confidence badges) settled this; retained as the rationale for that treatment.
