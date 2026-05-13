# Balanze

## Overview

Balanze is a local-first desktop utility for tracking personal AI usage across multiple providers in one place. The initial target is a tray-first desktop app built with Tauri 2 and Rust for Windows 11 and macOS 15+, with Ubuntu 24.04 LTS+ on GNOME added in a later phase and a future path to Android and a hosted web dashboard.

The product goal is simple: reduce the friction of checking multiple tools, tabs, billing pages and account surfaces to answer a few recurring questions — How much of Claude has been used today? How much OpenAI API credit remains? When does a limit reset? How close is the current account to budget or cap?

This is a side project, so the product should optimize for usefulness, low maintenance, and tight scope rather than maximum provider coverage on day one.

## Problem

Heavy AI users increasingly split work across consumer subscriptions and API accounts. A single person may use ChatGPT for interactive work, Claude for coding or long-form reasoning, and one or more API accounts for automation, while each platform exposes usage, billing, credits and limits in different ways.

Today, the available tooling is fragmented. Existing open-source tools are often Claude-specific, CLI-only, IDE-bound, or focused on developer observability rather than personal cross-provider usage monitoring.

This creates three user problems:

- Time wasted checking multiple sources manually.
- Poor awareness of reset windows, credit depletion, and spend trends.
- No single normalized view across subscription usage and API usage.

## Users

### Primary user

An individual power user, developer, founder, product manager, or researcher who actively uses more than one AI platform and wants lightweight visibility without running a SaaS admin product.

### Secondary user

A small team or technically inclined individual who wants a local dashboard and alerting layer for personal or shared accounts, but not full enterprise observability.

## Goals

### Product goals

- Provide one desktop app that shows AI usage across multiple providers in a normalized way.
- Support both subscription-style usage and API billing or credit usage where feasible.
- Make the app useful from the tray or menu bar with minimal interaction.
- Keep all core functionality local-first and transparent.
- Preserve a clean path to later add Android and a hosted dashboard without rewriting the core architecture.

### MVP goals

The MVP is the eventual smallest-viable end-state described in the rest of this document. The Phasing section below splits delivery into v0.1, v0.2, v0.3, and v1+ stages.

- Ship on Windows 11, macOS 15+, and Ubuntu 24.04 LTS+ GNOME.
- Support at least OpenAI and Anthropic as the first two providers.
- Show current usage snapshot, reset timing where available, spend or credits where available, and lightweight recent history.
- Allow threshold alerts for spend, quota, or estimated remaining usage.

### Non-goals

- Full enterprise cost allocation or multi-seat observability.
- Native OS widgets in the initial roadmap.
- Broad Linux desktop support beyond Ubuntu GNOME.
- Browser automation or brittle scraping as a headline feature.
- Monetization, subscriptions, cloud sync, or team billing in the first version.

## Product principles

- Local-first by default.
- Honest about data quality and source provenance.
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
| Windows 11 | Phase 1 (v0.1) | Desktop tray-first experience. Windows 10 excluded. |
| macOS 15+ | Phase 1 (v0.1) | Menu-bar-first experience with hidden main window that opens via tray. |
| Ubuntu 24.04 LTS+ GNOME | Phase 3 (v0.3) | GNOME only, with AppIndicator tray support. Deferred until the Win + Mac story is mature; Linux tray fragility makes it the wrong place to start. |
| Android | Phase 4+ (v1+) | Companion app only after desktop proves value. Read-only feed of the desktop state via Phase 3's sync layer. |
| Hosted web dashboard | Phase 4+ (v1+) | Separate surface for wallboard or TV use, reusing normalized backend contracts. |

## Key use cases

- See all major AI accounts and balances in one glance.
- Check Claude subscription usage and reset timing without opening the web app, where reliable data is available.
- Check OpenAI API spend and remaining credits quickly.
- Get notified when nearing a usage limit or spending threshold.
- Compare current billing-cycle usage across providers.
- Open a deeper dashboard for trends and account details.

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
- Confidence level: exact, partial, estimated.

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

### Transparency

Every provider metric should expose its source and precision. For example, an exact API credit balance should not be displayed in the same way as an estimated subscription remaining value.

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

## Competitive landscape

Current open-source tools validate demand but do not fully cover the proposed product shape. Existing examples include Claude-specific taskbar or menu-bar apps, local usage monitors, and CLI analyzers such as CodeZeno's Claude Code Usage Monitor, IgniteStudiosLtd's claude-usage-tool, usage-monitor-for-claude, and ccusage.

The main market gap appears to be a local-first, open-source, multi-provider desktop utility that combines subscription visibility and API billing or credit visibility in one normalized experience.

## Success criteria

For an MVP side project, success should be measured qualitatively and with a few lightweight quantitative checks:

- The app becomes the default place to check AI usage daily.
- Two providers work reliably enough for personal use.
- The tray or compact popup answers the core status question in a few seconds.
- Setup time for one provider is under 10 minutes.
- Alerting reduces surprise credit depletion or quota exhaustion (Phase 2 onward; v0.1 ships without alerts).

## Phasing

The MVP described above lands across four phases. v0.1 is the current build target; later phases respond to v0.1 usage and the design's deferred work.

### Phase 1 — v0.1 (current target)

Tightest possible side-project ship. The goal is to replace the user's existing fragmented setup with one local-first binary on Windows and macOS. Detailed architecture and step-by-step build sequence live in the design doc; this is the product-level summary.

- Tauri 2 + Rust + Svelte 5 desktop app. Windows 11 and macOS 15+ only (Ubuntu deferred to Phase 3).
- Claude subscription utilization + reset clock via Anthropic's OAuth usage endpoint (`GET api.anthropic.com/api/oauth/usage`, Bearer from `~/.claude/.credentials.json`). This is the authoritative signal — replaces what would otherwise have been a multi-week empirical heuristic. Searched in both `~/.claude/` and `~/.config/claude/` (Claude Code v1.0.30+ uses the latter on some platforms).
- Per-event detail (per-model breakdown, sparkline, burn rate) via local JSONL parsing of `<claude_home>/projects/**/*.jsonl` — no API, no scraping, no auth. Approach proven by ccusage; reimplemented in Rust to keep the binary self-contained. Events deduped by `(message_id, request_id)` to avoid double-counting retries and subagent forwarding.
- Claude API spend + extra credits via Anthropic Console — **deferred to v0.2** after the step-1 spike. The Console (platform.claude.com) does expose stable JSON endpoints, but auth is via expiring session cookies rather than an API key; cookie-paste UX deserves first-class design and a `DegradedState::auth_expired` flow, neither of which fits in v0.1's scope. v0.1 ships without Anthropic API spend visibility on the Console side; subscription usage from `~/.claude/projects/` JSONL is still fully covered.
- OpenAI API spend + credits via documented billing endpoints.
- Tray icon as a color-shifting gauge (idle → green → amber → red) with hover tooltip; macOS additionally shows a `Title` text slot with the current percentage.
- Tray menu: Open Balanze / Settings… / Quit.
- Main window (popover-shaped, hidden on launch): one progress bar per Anthropic-exposed reset cadence (5h, 7-day, plus any model-specific bars like "Sonnet only" if OAuth exposes them) each with its own "Resets in …" subline; sparkline of recent burn; predictive reset countdown ("~42 min to cap" with confidence band, or "uncertain" / "??" during warm-up); OpenAI spend tile.
- Predictive reset is the headline feature: EWMA over the rolling window with an explicit warm-up state (Insufficient → Uncertain → Confident) so the predictor never lies immediately after a window reset.
- Settings UI: paste API keys, save to OS keychain.
- Local secure storage (keychain for secrets; `directories`-crate per-OS paths for logs and non-secret settings).
- `tauri-plugin-single-instance` wired from day one so the user cannot accidentally double-launch.
- Distribution: unsigned MSI/NSIS (Windows) + DMG/app (macOS) via GitHub Releases. No auto-update in v0.1; users download new versions manually.
- No alerts in v0.1. Learn from real use first; alerts arrive in v0.2 with thresholds informed by observation.
- No full dashboard window in v0.1. The popover covers the read use case; the dashboard arrives in v0.2.

### Phase 2 — v0.2 (post-v0.1 completion + polish)

Round out what was deferred from v0.1 and respond to the first weeks of real use.

- Alerts for spend thresholds, credits below threshold, subscription approaching cap, reset windows, and connector failure / stale data — with thresholds informed by v0.1 observation.
- Optional full dashboard window for per-provider detail pages, recent trends, account source and confidence indicators, settings, connectors, and troubleshooting.
- Auto-update via Tauri updater (pointed at the GitHub Releases JSON manifest).
- Anthropic Console integration via session-cookie paste-from-DevTools flow (endpoints already discovered and documented in the design doc's Open Questions section). Tile shows `auth_expired` state on 401 and prompts the user to re-paste cookies.
- "Send Logs" tray menu item that bundles rotated logs + recent state snapshot for support.
- Subscription "estimated $ value" tile (synthetic-dollar display via a hardcoded pricing table — kept separate from the cap math, which stays denominated in tokens).
- Code signing investigation (Windows certificate, macOS notarization) if external users start appearing.
- Integration robustness improvements informed by real failures observed in v0.1.

### Phase 3 — v0.3 (breadth + cross-device)

Features that need a base of confidence on the desktop story before expanding outward.

- Ubuntu 24.04 LTS+ GNOME support — the third tier from the original PRD, deferred until the Windows + macOS experience is solid because GNOME tray behavior is the most fragile of the three.
- Cross-device sync via a small relay (GitHub Gist / Cloudflare KV / iCloud Drive) — one Balanze identity reads and writes the same numbers across devices. Sets up Android + hosted dashboard cleanly.
- Export and snapshot reporting.
- Additional provider connectors as repeated personal need surfaces (Gemini, Cursor, OpenRouter, others).

### Phase 4+ — v1.0 and beyond

The original PRD's long-tail vision, contingent on the product proving sticky enough on the desktop to justify mobile and hosted surfaces.

- Android companion app — read-only feed of the desktop's state via Phase 3's sync layer.
- Hosted web dashboard for wallboard / TV use, reusing the normalized backend contracts.

## Open questions

Items still genuinely unresolved at the product level. The design doc carries the technical open questions and spike plan.

- Which provider metrics are realistically obtainable through stable official integration versus inference? Step-1 spike of v0.1 will resolve the Anthropic Console question concretely; broader provider coverage in later phases will need similar investigation per provider.
- Should onboarding ask users to choose a trust mode, such as "official only" versus "include estimated subscription metrics"? Deferred — v0.1 marks every metric with its source per the Transparency requirement, which may make an explicit trust-mode setting unnecessary.
- How much locally-stored history is needed before the product becomes meaningfully better than a simple current-status tool? v0.1 keeps a rolling-window-sized in-memory history; SQLite persistence is deferred to v0.2 unless startup re-parse latency becomes a real problem.
- What is the right default alert threshold mix (Phase 2)? Decide after observing real-use patterns in v0.1 — premature thresholds are noise.
