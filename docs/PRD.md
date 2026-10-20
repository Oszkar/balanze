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
- Keep shared domain logic reusable without building speculative Android or hosted infrastructure.
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
- Periodic background refresh and a manual display refresh using the latest known snapshot; on-demand provider polling is not implemented.
- Local secure storage for credentials and preferences. (Durable history is deferred until a concrete consumer needs it; CLI `export` re-derives history from JSONL statelessly.)
- Runnable distribution: downloadable binaries, no Rust toolchain required (v0.5).

### Out of scope

- Android app.
- Hosted dashboard.
- Collaboration features.
- Team analytics and seat breakdowns.
- Provider marketplace with dozens of integrations.
- Native widgets.

## Supported platforms

| Platform | Architecture | Support level | Artifacts | Notes |
|---|---|---|---|---|
| Windows 11 | x64 | CLI v0.1; tray UI v0.3 | Desktop + CLI | Tray-first desktop experience lands across the v0.3 sub-milestones (popover in v0.3.0). Windows 10 excluded. Installers are unsigned by decision - see "Code signing" below. |
| Windows 11 | arm64 | CLI v0.5.1 | CLI only | The CLI cross-compiles cleanly (pure Rust plus rustls). The desktop app is not built for arm64: it needs a verified WebView2 runtime story and a runner decision, tracked separately. x64 desktop builds run under emulation meanwhile. |
| macOS 15+ | Apple Silicon (arm64) | CLI v0.1; tray UI v0.3 | Desktop + CLI | Menu-bar-first experience with hidden main window across v0.3. Release DMG/app signed and notarized from v0.5.0. The CLI archive is unsigned; Homebrew is the recommended path. |
| macOS, Intel | x86_64 | **Excluded** | None | No release artifact is built. macOS 15 already drops most Intel hardware, so a universal binary would double mac build time and bundle size to serve a shrinking set of machines that largely cannot run the documented OS floor. `cargo install` from source is untested but unobstructed. |
| Linux (generic) | x64 | CLI v0.1; binaries v0.5.1 | CLI only | A statically linked musl binary from v0.5.1, so it runs on any distro regardless of glibc age. No OS credential store is wired on Linux - the OpenAI key comes from `BALANZE_OPENAI_KEY`. Tray UI not targeted here. |
| Ubuntu 24.04 LTS+ GNOME | x64 | Future | - | GNOME tray UI with AppIndicator support. Deferred until the Win + Mac tray story is mature; Linux tray fragility makes it the wrong place to start. |

The architecture column is load-bearing: it must track `release.yml`'s build matrices and `deny.toml`'s `[graph].targets` exactly. `deny.toml` carries the **union** of the desktop and CLI matrices. Adding an architecture means touching all three.

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

One-shot CLI status and the live tray/watch surfaces share source-selection and derivation rules. Status honors existing provider toggles before reading credentials or calling providers, while local Claude JSONL/statusline reads remain available. Fresh statusline quota takes precedence over OAuth; OAuth still supplies extra-usage billing. Status and export preserve valid complete JSONL records around malformed lines and unfinished tails. Export remains an explicit full-history read. These paths remain independent observations, so their values can differ when their source inputs or evaluation times differ.

The headline view is a 2×2 matrix - Anthropic / OpenAI × subscription **quota %** / real billed **$** - and it follows one rule that the whole honesty story rests on: **the matrix holds measured reality only.**

A cell is filled only by a number that was actually measured or actually billed:

- **Anthropic quota %** - fresh statusline quota, with OAuth fallback (real).
- **OpenAI quota %** - Codex CLI rate-limit % (real).
- **OpenAI billed $** - Admin Costs API (real).
- **Anthropic billed $** - the `extra_usage` pay-as-you-go overage, *only* if the user enabled it on claude.ai (real, exact cents). Otherwise the cell reads as honestly **unavailable** ("Anthropic exposes no per-user API spend") - it is never backfilled with a substitute number. Recovering real Anthropic API spend/balance stays an open future item.

**Estimates and counterfactuals never occupy a matrix cell.** The current-calendar-month list-price figure derived from local JSONL - what the same Claude Code usage *would* cost at API list prices - is presented separately as a **"Subscription leverage"** insight: explicitly a value/counterfactual stat ("leverage you got from the subscription"), never billed, visually and structurally distinct from the grid.

Rationale: a grid asserts that cells in the same column are the same kind of number. Putting a counterfactual list-price estimate in the same column as real billed spend claims a comparability that does not exist - which reads as misleading even when the text is honest. Keeping the matrix measured-only makes the honesty **structural** rather than dependent on labels. This contract governs both the CLI compact view and the v0.3 popover; the `--json` schema is unaffected (every money cell already carries `source`/`confidence`, so machine consumers disambiguate from the wire shape).

### Measurement quality

Claude API-rate estimates use separate 5-minute and 1-hour cache-write counts when present in local logs; legacy logs use the 5-minute estimate. Cache duration never changes the aggregate token count. These are current list-rate estimates, not reconstructed invoices.

Anthropic quota is current only when its source timestamp is no more than 15 minutes old and not future-dated, no reported window has passed its reset, and the source has no error. Fresh statusline takes precedence over fresh OAuth. When neither is fresh, the last known quota remains visible with a stale label and no pace. The existing refresh cadence updates this status without pretending a new provider fetch occurred.


### Integrations

#### OpenAI

The product should support API usage and billing-related visibility using official surfaces where available, including usage and dashboard concepts already documented by OpenAI.

#### Anthropic

The product should support Claude-related usage views across API billing and subscription usage where reliable access is available. Claude Code owns its OAuth credential in every storage form; Balanze reads it without refreshing, modifying, mirroring, or backing it up, and directs the user to `claude login` when it expires. An explicit file-refresh opt-in may be considered during settings and configurability work, but must remain off by default and can never make the macOS Keychain source writable. Claude documents subscription usage and length limits as dynamic and plan-dependent, so the app should present these carefully and avoid overstating precision.

### Alerts (deferred)

Candidate notifications for a later release, subject to reliable source data and an actionable user need:

- Spend exceeds threshold.
- Credits remaining below threshold.
- Subscription usage nearing cap.
- Reset window approaching.
- Connector failure or stale data.

### Storage

- Store credentials locally using OS-appropriate secure storage when possible.
- Store settings locally. Export re-derives Claude history from local JSONL; durable history storage is deferred.
- No mandatory cloud backend.

## UX requirements

- The compact popup should open fast and be glanceable in under 5 seconds.
- The tray or menu-bar state should communicate utilization magnitude, with configurable combined or separate provider indicators planned for v0.6.
- Visual design should remain functional and lightweight rather than dashboard-heavy.
- Users should understand at a glance what is official data versus inferred data.

## Technical approach

Tauri provides the desktop shell, Rust owns shared domain logic and provider integration, and Svelte provides the popover and settings UI. The [architecture document](ARCHITECTURE.md) owns the crate map, write boundaries, IPC contract, and supervision rules. The roadmap does not authorize changes to those contracts without the review required by [AGENTS.md](../AGENTS.md).

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
- Tray utilization and provider identity are legible without opening the popover (v0.6). Notifications are deferred.

Engineering bar (what "finished-feeling" means here):

- The tray UI reads as intentional and credible, not a scaffold - clean enough that a screenshot stands on its own.
- The source/confidence model is legible at a glance: an onlooker can tell real billed spend from a quota % from a counterfactual estimate without reading documentation.
- User documentation explains provenance and measured pace; the architecture document explains implementation boundaries without duplicating the user guide.
- Someone can download a release and run it without a Rust toolchain (lightweight distribution, v0.5).

## Roadmap

Current release: **v0.5.3**. The next two releases focus on window consistency and glanceability. Alerts move to a later, unassigned release. Settings work needed for tray choices belongs to v0.6; broader customization is not a prerequisite for v1.0. The v1.0 readiness bar will be revisited after v0.6.

### Shipped

Historical details and implementation rationale live in the [CHANGELOG](../CHANGELOG.md) and [architecture document](ARCHITECTURE.md).

| Releases | Delivered |
|---|---|
| v0.1 | Four-quadrant data layer and CLI, with measured billing separate from subscription leverage. |
| v0.2 | Live updates, statusline bridge, and measured quota-versus-elapsed pace. |
| v0.3 | Tauri popover, settings, provider controls, and visible provenance/degradation. |
| v0.4 | Polished GUI, CLI, and statusline; Codex window support; reliability hardening. |
| v0.5.0-v0.5.2 | Desktop and CLI binaries, macOS signing, launch-at-login, Homebrew, and the docs site. |
| v0.5.3 | Cross-process coordination, ingestion, freshness, pricing, and presentation correctness fixes. |

### v0.5.4 - Window consistency & maintenance

A bounded maintenance release with three deliverables:

- **Codex window parity** ([#294](https://github.com/Oszkar/balanze/issues/294)). The maintainer currently sees only `Codex 7d · prolite` in the Tauri popover on Windows and macOS. Trace ingestion and presentation to identify the cause. Show distinct 5-hour and 7-day windows when the account-wide source supplies both, matching Claude's treatment; do not fabricate absent data. Check the CLI, TUI, and statusline for the same omission while respecting compact surface formats.
- **Consistent window labels** ([#280](https://github.com/Oszkar/balanze/issues/280)). Normalize known and unknown window labels across surfaces, retain model-specific labels where applicable, and cover the policy with shared fixture vectors.
- **Maintainer price refresh** ([#277](https://github.com/Oszkar/balanze/issues/277)). Automate the pinned LiteLLM snapshot refresh with a reviewable rate diff and missing-model checks. Official-price cross-checking remains manual; no runtime fetching or scheduled refresh.

Done when the reported window omission is explained and corrected where data exists, label parity is covered, and a maintainer can run the documented refresh workflow. Diagnostics bundling is deferred.

### v0.6 - Glanceable tray & menu bar

Make utilization and provider identity useful without opening the popover.

- **Utilization icon and provider modes** ([#295](https://github.com/Oszkar/balanze/issues/295)). Compare a filling square/bar and a circular gauge at actual tray sizes. Offer a user setting for one most-constrained indicator or separate Claude/Codex indicators. The maintainer prefers seeing both. One shared icon versus separate tray icons, the default mode, and which window drives each provider remain design decisions.
- **Compact macOS text** ([#296](https://github.com/Oszkar/balanze/issues/296)). Replace the space-heavy fully spelled-out provider text with a compact presentation. Evaluate short provider labels and optional percentages/icon-only display in a realistically crowded menu bar; exact options remain open.
- **Consistent passive urgency** ([#263](https://github.com/Oszkar/balanze/issues/263)). Define how quota severity, pace, stale data, and source errors interact across surfaces, with intentional differences documented and representative parity coverage. Passive indicators are distinct from OS notifications.

Done when utilization magnitude and provider identity are readable at native Windows/macOS tray sizes, users can select combined or separate provider presentation, macOS text is materially shorter, and missing/stale/error states remain honest. Preserve deduped repainting. Settings/schema/capability and architecture details still require the normal design review before implementation.

OS notifications, broad theming, and multi-environment aggregation are outside this release. The broad presentation refactor in [#232](https://github.com/Oszkar/balanze/issues/232) needs re-audit against shipped fixes; it is not automatically a prerequisite for either release.

### Later - unassigned

These are candidates, not a release sequence or prerequisites for v1.0:

- **Multiple local environments** ([#297](https://github.com/Oszkar/balanze/issues/297)): investigate one Windows host plus WSL2 instances using the same Claude/OpenAI accounts. WSL-only activity must eventually keep the host view current; concurrent activity should contribute complete aggregates without duplicate records. Account-wide quota observations must be reconciled, never summed. First compare additional read locations with a local bridge and define freshness, identity, lifecycle, and deduplication requirements. Cloud sync and different-account support are not requirements for this investigation.
- **Alerts**: choose actionable quota, reset, spend, or persistent-failure notifications based on real use. Require fresh data, duplicate suppression, and defined startup/re-arm behavior. Credits alerts depend on a reliable balance source. No notification thresholds are committed yet.
- **Save Diagnostics...** ([#278](https://github.com/Oszkar/balanze/issues/278)): revisit when collecting support information solves a concrete need; contents, redaction, packaging, and CLI access remain open.
- **Additional configuration**: themes, statusline controls in the UI, refresh cadence within provider floors, and startup behavior, as actual needs emerge.
- **CLI improvements**: export filtering ([#114](https://github.com/Oszkar/balanze/issues/114)), JSON time-series export ([#247](https://github.com/Oszkar/balanze/issues/247)), and typed failure categories ([#279](https://github.com/Oszkar/balanze/issues/279)). These do not fit either release's theme; typed status errors need separate schema review.
- **Platform ergonomics**: Linux credential storage ([#231](https://github.com/Oszkar/balanze/issues/231)) and Windows arm64 desktop artifacts ([#219](https://github.com/Oszkar/balanze/issues/219)). Windows arm64 CLI artifacts already ship.
- **Dashboard/history, extra providers, Ubuntu GNOME desktop, cross-device sync, and Android** remain uncommitted. Durable history needs a concrete consumer before adding storage. WinGet and auto-update remain optional; Windows signing remains declined as documented above.

The Vite development-server issue ([#136](https://github.com/Oszkar/balanze/issues/136)) remains a development impediment to track independently, not a promised product feature. Explicit Claude file-credential refresh ([#186](https://github.com/Oszkar/balanze/issues/186)) remains uncommitted and requires its own credential-boundary review; the shipped read-only policy stands.

## Open decisions

- Which tray geometry remains legible at native sizes, and should separate providers use separate icons?
- Which window drives each provider's indicator, how are ties handled, and how do stale/error states interact with utilization?
- What compact macOS text options and default best balance identity, information, and width?
- For host/WSL shared accounts, what collection mechanism yields fresh quotas and complete activity without duplicate records or additional provider traffic?
- After v0.6, what concrete readiness criteria remain for v1.0?
