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

The MVP is the eventual smallest-viable end-state described in the rest of this document. The Phasing section below splits delivery into v0.1, v0.2, v0.3, v0.4, and v1+ stages (themes: Data → Liveness → UI → Distribution → long tail).

- Ship the desktop story on Windows 11 and macOS 15+ (CLI in v0.1 → tray UI in v0.3); Ubuntu 24.04 LTS+ GNOME follows in v1+ — see the Supported platforms table and Phasing for the per-stage rollout.
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
| Windows 11 | CLI v0.1; tray UI v0.3 | CLI works from v0.1 (`cargo install`); tray-first desktop experience arrives in the v0.3 UI phase. Windows 10 excluded. |
| macOS 15+ | CLI v0.1; tray UI v0.3 | CLI works from v0.1; menu-bar-first experience with hidden main window in v0.3. |
| Linux (generic) | CLI v0.1 | `cargo install` works trivially; no separate test matrix. CLI only — tray UI not targeted here. |
| Ubuntu 24.04 LTS+ GNOME | Phase 5+ (v1+) | GNOME tray UI with AppIndicator support. Deferred until the Win + Mac tray story is mature; Linux tray fragility makes it the wrong place to start. |
| Android | Phase 5+ (v1+) | Companion app only after desktop proves value. Read-only feed of the desktop state via the sync layer. |
| Hosted web dashboard | Phase 5+ (v1+) | Separate surface for wallboard or TV use, reusing normalized backend contracts. |

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
- Alerting reduces surprise credit depletion or quota exhaustion (v0.3 onward, configured in the settings UI; v0.1–v0.2 ship without alerts).

## Phasing

The MVP lands across four release phases plus a long tail. Each phase has **one dominant theme** — Data → Liveness → UI → Distribution — so "done" for each is hard to fudge and risk is sequenced correctly (read-only data primitives first, asymmetric/UI work later). This replaces an earlier phasing that tried to ship the full tray UI + predictor in v0.1; the design doc's roadmap review (Approach A) re-scoped v0.1 down to a complete, honest data layer after the Anthropic Admin API spike came back NO-GO for non-enterprise accounts. The detailed build sequence lives in the design doc; this is the product-level summary.

### Phase 1 — v0.1: Data (shipped, pre-tag)

A complete, honest **data layer** exposed as a CLI (`balanze-cli`). No tray UI yet — the CLI prints the same normalized snapshot the eventual popover will show. The bar for v0.1 is the **four-quadrant matrix fully lit**:

| | Quota % | API $ |
|---|---|---|
| **Anthropic** | OAuth usage endpoint (5h / 7-day / per-model cadence bars + reset clocks) | estimated, JSONL-derived (see below) |
| **OpenAI** | Codex CLI rate-limit % (local `~/.codex/sessions/` rollout files) | real billed spend (Admin Costs API) |

- Claude subscription utilization + reset clock via Anthropic's OAuth usage endpoint (`GET api.anthropic.com/api/oauth/usage`, Bearer from `~/.claude/.credentials.json`). Authoritative signal; no scraping. Searched in both `~/.claude/` and `~/.config/claude/`.
- Per-event detail (per-model breakdown, burn rate, rolling window) via local JSONL parsing of `<claude_home>/projects/**/*.jsonl` — no API, no scraping, no auth. Events deduped by `(message_id, request_id)`.
- **Anthropic API $ is an estimate, not real spend.** The Phase 0 spike confirmed Anthropic's official Usage & Cost API is gated to enterprise/org-admin accounts — inaccessible to the modal v0.1 user. Instead `claude_cost` synthesizes a list-price equivalent from the same JSONL × a vendored LiteLLM price table. For Pro/Max users this is "subscription leverage," **not** money billed; the CLI labels it as such and never presents it next to the real OpenAI bill without that distinction. (A real-spend source via the Admin API is a v0.2+ research note, contingent on the author obtaining enterprise access or a user requesting it.)
- OpenAI Codex quota % from the local Codex CLI rollout files (`~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-*.jsonl`) — server-computed `rate_limits.primary`, a real number.
- OpenAI API spend via the documented Admin Costs API (`GET /v1/organization/costs`, `sk-admin-…` Bearer).
- `balanze-cli setup` — interactive wizard: checks Anthropic OAuth presence, checks Codex sessions, prompts for the OpenAI admin key (masked input), validates it live, stores it in the OS keychain. No Anthropic admin-key prompt (no admin API in v0.1).
- `--sections` (per-source detail) and `--json` (machine-readable snapshot) output modes — flags on `status`, and also accepted as bare top-level shortcuts (`balanze-cli --sections` / `balanze-cli --json`). (Originally slated for v0.2 but shipped early in v0.1 alongside the 4-quadrant integration.)
- Local secure storage (keychain for secrets; `directories`-crate per-OS paths for non-secret settings). Known Windows keychain limitation documented, with a `BALANZE_OPENAI_KEY` env-var fallback.
- **Distribution: source only.** `cargo install --git https://github.com/Oszkar/balanze balanze_cli` (the repo root is a virtual workspace, so the package is named explicitly; it builds the `balanze-cli` binary). No binaries, no installers, no GitHub Releases in v0.1 — the audience (org-admin tinkerer power-users) accepts the Rust-toolchain prerequisite. Linux works via `cargo install` (no separate test matrix; tray UI is later anyway).
- **Not in v0.1:** tray UI, popover, predictor, file watcher, alerts, dashboard window. All deliberately moved to later phases.

### Phase 2 — v0.2: Liveness

Make the data update itself and project forward. No UI yet; the CLI gets "alive."

- File watcher (`notify` + debounce + a safety poll) so JSONL-derived numbers update without a manual re-run.
- Predictive reset: EWMA over the rolling window with an explicit warm-up state machine (Insufficient → Uncertain → Confident) so the predictor never lies immediately after a window reset.
- `--watch` (long-running refresh loop) and a `statusline` mode for shell prompts / status bars.
- Integration robustness improvements informed by the first weeks of real v0.1 use.

### Phase 3 — v0.3: UI

The Tauri surface, and the secret-storage and provider work that naturally rides with a real settings screen.

- Tauri 2 popover/tray UI: color-shifting gauge tray icon, hidden-on-launch popover with one progress bar per Anthropic cadence + reset sublines, burn sparkline, the predictive countdown from v0.2, and the 4-quadrant tiles. `tauri-plugin-single-instance` wired so the user cannot double-launch.
- Settings UI: paste API keys, save to OS keychain — and with a real key-input box the keychain code is exercised on both platforms, so this is where the **`keyring` → `keyring-core` (v4) migration** lands (fixes the v0.1 Windows keychain no-op).
- Degraded-state events surfaced visually (stale data shown with a warning rather than blanked).
- Optional full dashboard window: per-provider detail, recent trends, source/confidence indicators, troubleshooting.
- Alerts for spend thresholds, credits/quota below threshold, subscription approaching cap, reset windows, and connector failure / stale data — thresholds informed by v0.1–v0.2 observation, configured in the new settings UI.
- Anthropic Console integration via the session-cookie paste-from-DevTools flow (cookie-paste UX needs the UI; tile shows an `auth_expired` state and prompts a re-paste on 401).

### Phase 4 — v0.4: Distribution

Make it installable without a Rust toolchain.

- Signed binaries: Windows code-signing certificate, macOS notarization.
- Packaged installers (MSI/NSIS, DMG/app) via GitHub Releases; Homebrew tap and WinGet manifest.
- Auto-update via the Tauri updater pointed at the Releases JSON manifest.
- "Send Logs" menu item bundling rotated logs + a recent state snapshot for support.

### Phase 5+ — v1.0 and beyond

The long-tail vision, contingent on the product proving sticky on the desktop.

- Ubuntu 24.04 LTS+ GNOME support — deferred until the Windows + macOS experience is solid (GNOME tray behavior is the most fragile of the three).
- Cross-device sync via a small relay (GitHub Gist / Cloudflare KV / iCloud Drive) — one Balanze identity reading/writing the same numbers across devices; sets up Android + hosted dashboard cleanly.
- Additional provider connectors as repeated personal need surfaces (Gemini, Cursor, OpenRouter, others); export/snapshot reporting.
- Android companion app — read-only feed of the desktop's state via the sync layer.
- Hosted web dashboard for wallboard / TV use, reusing the normalized backend contracts.

## Open questions

Items still genuinely unresolved at the product level. The design doc carries the technical open questions and spike plan.

- Which provider metrics are realistically obtainable through stable official integration versus inference? The v0.1 Phase-0 spike resolved the Anthropic side (official Usage & Cost API is enterprise/admin-gated → NO-GO for the modal user; JSONL-derived estimate ships instead). The Anthropic Console cookie-paste path is a v0.3 research item; broader provider coverage in later phases needs similar per-provider investigation.
- Should onboarding ask users to choose a trust mode, such as "official only" versus "include estimated subscription metrics"? Deferred — v0.1 marks every metric with its source per the Transparency requirement, which may make an explicit trust-mode setting unnecessary.
- How much locally-stored history is needed before the product becomes meaningfully better than a simple current-status tool? v0.1 keeps a rolling-window-sized in-memory history; SQLite persistence is deferred to v0.2 unless startup re-parse latency becomes a real problem.
- What is the right default alert threshold mix (alerts land in v0.3)? Decide after observing real-use patterns through v0.1–v0.2 — premature thresholds are noise.
