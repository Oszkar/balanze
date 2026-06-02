# Balanze

## Overview

Balanze is a local-first desktop utility for tracking personal AI usage across multiple providers in one place. The initial target is a tray-first desktop app built with Tauri 2 and Rust for Windows 11 and macOS 15+, with Ubuntu 24.04 LTS+ on GNOME added in a later phase and a future path to Android and a hosted web dashboard.

The product goal is simple: reduce the friction of checking multiple tools, tabs, billing pages and account surfaces to answer a few recurring questions — How much of Claude has been used today? How much OpenAI API credit remains? When does a limit reset? How close is the current account to budget or cap?

This is a side project, so the product should optimize for usefulness, low maintenance, and tight scope rather than maximum provider coverage on day one.

## Project intent

Balanze is a side project held to a high engineering bar — the kind of tool the author wants to use daily and keep polishing. That framing, more than any user-acquisition goal, drives the prioritization in this document:

- **Quality over reach.** Effort goes into a clean, honest, well-tested core and a credible UI rather than maximizing user count. Distribution stays deliberately lightweight (a runnable release, not an app-store presence).
- **Narrow but deep.** Two providers done honestly beats six done shabbily. Broad provider coverage is a "someday," not a near-term goal.
- **Honesty is a first-class feature.** Every metric carries its source and confidence, and the UI makes that visible. The data-provenance model is treated as part of the product surface, not a footnote — see the matrix rules below.
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

### MVP goals

The MVP is the eventual smallest-viable end-state described in the rest of this document. The Phasing section below splits delivery into v0.x stages plus an uncommitted Vision tier (themes: Data → Liveness → UI → Distribution & Legibility → Vision). Each increment is shippable on its own.

- Ship the desktop story on Windows 11 and macOS 15+ (CLI in v0.1 → tray UI across the v0.3 sub-milestones); Ubuntu 24.04 LTS+ GNOME is Vision-tier — see the Supported platforms table and Phasing for the per-stage rollout.
- Support at least OpenAI and Anthropic as the first two providers.
- Show current usage snapshot, reset timing where available, spend or credits where available, and lightweight recent history.
- Allow threshold alerts for spend, quota, or estimated remaining usage.

### Non-goals

- Full enterprise cost allocation or multi-seat observability.
- Native OS widgets in the initial roadmap.
- Broad Linux desktop support beyond Ubuntu GNOME.
- Browser automation or brittle scraping as a headline feature.
- Monetization, subscriptions, cloud sync, or team billing in the first version.
- Heavyweight distribution (code-signing, notarization, app-store/package-manager presence, auto-update) as a committed goal — distribution stays lightweight (a runnable release); the signing/store work is optional, not a phase the roadmap depends on.
- Broad provider coverage — the product stays narrow-but-deep; additional connectors are Vision-tier.

## Product principles

- Local-first by default.
- Honest about data quality and source provenance.
- **Show measured status, not forecasts.** Present what *is* — how much is used and how far through the window you are — and let the user judge what comes next. Applies to money (the measured-only matrix) and to time (pace, not a "you'll run out at…" prediction).
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
| Linux (generic) | CLI v0.1 | `cargo install` works trivially; no separate test matrix. CLI only — tray UI not targeted here. |
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

### The usage matrix (presentation contract)

The headline view is a 2×2 matrix — Anthropic / OpenAI × subscription **quota %** / real billed **$** — and it follows one rule that the whole honesty story rests on: **the matrix holds measured reality only.**

A cell is filled only by a number that was actually measured or actually billed:

- **Anthropic quota %** — server-authoritative OAuth utilization (real).
- **OpenAI quota %** — Codex CLI rate-limit % (real).
- **OpenAI billed $** — Admin Costs API (real).
- **Anthropic billed $** — the `extra_usage` pay-as-you-go overage, *only* if the user enabled it on claude.ai (real, exact cents). Otherwise the cell reads as honestly **unavailable** ("Anthropic exposes no per-user API spend") — it is never backfilled with a substitute number. Recovering real Anthropic API spend/balance stays an open future item.

**Estimates and counterfactuals never occupy a matrix cell.** The list-price figure derived from local JSONL — what the same Claude Code usage *would* cost at API list prices — is presented separately as a **"Subscription leverage"** insight: explicitly a value/counterfactual stat ("leverage you got from the subscription"), never billed, visually and structurally distinct from the grid.

Rationale: a grid asserts that cells in the same column are the same kind of number. Putting a counterfactual list-price estimate in the same column as real billed spend claims a comparability that does not exist — which reads as misleading even when the text is honest. Keeping the matrix measured-only makes the honesty **structural** rather than dependent on labels. This contract governs both the CLI compact view and the v0.3 popover; the `--json` schema is unaffected (every money cell already carries `source`/`confidence`, so machine consumers disambiguate from the wire shape).

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

For a personal, engineering-quality-first side project, success is mostly qualitative, with a few lightweight checks. Some criteria are about daily usefulness; some are about the bar the build is held to (see Project intent).

Usefulness:

- The app becomes the default place to check AI usage daily.
- Two providers work reliably enough for personal use.
- The tray or compact popup answers the core status question in a few seconds.
- Setup time for one provider is under 10 minutes.
- Alerting reduces surprise credit depletion or quota exhaustion (v0.3.2 onward, configured in the settings UI; v0.1–v0.3.1 ship without alerts).

Engineering bar (what "finished-feeling" means here):

- The tray UI reads as intentional and credible, not a scaffold — clean enough that a screenshot stands on its own.
- The source/confidence model is legible at a glance: an onlooker can tell real billed spend from a quota % from a counterfactual estimate without reading documentation.
- The codebase tells its own story: the architecture, the data-provenance model, and the "measured status, not forecasts" decision (why the EWMA predictor was built, dogfooded, and then retired in favour of honest pace facts) are explained well enough (README + a short "how it works" writeup) that someone can understand the interesting decisions without spelunking.
- Someone can download a release and run it without a Rust toolchain (lightweight distribution, v0.4).

## Phasing

The MVP lands across four release phases plus an uncommitted Vision tier. Each phase has **one dominant theme** — Data → Liveness → UI → Distribution & Legibility → Vision — so "done" for each is hard to fudge and risk is sequenced correctly (read-only data primitives first, asymmetric/UI work later). v0.3 (UI) is delivered as **bounded sub-milestones** (v0.3.0–v0.3.3) that each ship on their own, so the hero artifact — the popover — exists early rather than at the end of one large release. This summary supersedes an earlier phasing that tried to ship the full tray UI + predictor in v0.1, and a later one that packed all UI work into a single monolithic v0.3. The detailed build sequence lives in the design doc; this is the product-level summary.

### Phase 1 — v0.1: Data (shipped, pre-tag)

A complete, honest **data layer** exposed as a CLI (`balanze-cli`). No tray UI yet — the CLI prints the same normalized snapshot the eventual popover will show. The bar for v0.1 is the **four-quadrant matrix fully lit**:

| | Quota % | API $ (real billed) |
|---|---|---|
| **Anthropic** | OAuth usage endpoint (5h / 7-day / per-model cadence bars + reset clocks) | `extra_usage` overage if the user enabled pay-as-you-go (real); else **unavailable**. The JSONL list-price estimate is *not* in this cell — it is the separate "Subscription leverage" insight (see below) |
| **OpenAI** | Codex CLI rate-limit % (local `~/.codex/sessions/` rollout files) | real billed spend (Admin Costs API) |

Per the matrix presentation contract above, every cell holds measured reality only.

- Claude subscription utilization + reset clock via Anthropic's OAuth usage endpoint (`GET api.anthropic.com/api/oauth/usage`, Bearer from `~/.claude/.credentials.json`). Authoritative signal; no scraping. Searched in both `~/.claude/` and `~/.config/claude/`.
- Per-event detail (per-model breakdown, burn rate, rolling window) via local JSONL parsing of `<claude_home>/projects/**/*.jsonl` — no API, no scraping, no auth. Events deduped by `(message_id, request_id)`.
- **The Anthropic billed-$ cell shows real money or nothing.** Anthropic exposes no per-user API spend — the Phase-0 spike confirmed the official Usage & Cost API is enterprise/org-admin-gated (NO-GO for the modal user). So that cell holds the real `extra_usage` pay-as-you-go overage when the user enabled it, and otherwise reads as **unavailable** — never backfilled with a substitute. The list-price equivalent `claude_cost` synthesizes from local JSONL × a vendored LiteLLM price table is **not** spend; it is the separate **"Subscription leverage"** insight (what the same usage would cost at API list prices — leverage from the subscription, never billed). *v0.1 originally rendered that estimate inside the cell with a "not billed" label; moving it out of the matrix entirely — structural honesty instead of label-dependent — is the near-term CLI presentation reshape (R1), also baked into the v0.3.0 popover.* The premise that Claude Code records a per-event cost in the JSONL was **disproven** (verified absent, 790 files, 2026-05-19; see Phase 2 Track C). Recovering a real Anthropic balance via the org-gated Admin API or the demoted Console cookie-paste remains contingent on access.
- OpenAI Codex quota % from the local Codex CLI rollout files (`~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-*.jsonl`) — server-computed `rate_limits.primary`, a real number.
- OpenAI API spend via the documented Admin Costs API (`GET /v1/organization/costs`, `sk-admin-…` Bearer).
- `balanze-cli setup` — interactive wizard: checks Anthropic OAuth presence, checks Codex sessions, prompts for the OpenAI admin key (masked input), validates it live, stores it in the OS keychain. No Anthropic admin-key prompt (no admin API in v0.1).
- `--sections` (per-source detail) and `--json` (machine-readable snapshot) output modes — flags on `status`, and also accepted as bare top-level shortcuts (`balanze-cli --sections` / `balanze-cli --json`). (Originally slated for v0.2 but shipped early in v0.1 alongside the 4-quadrant integration.)
- Local secure storage (keychain for secrets; `directories`-crate per-OS paths for non-secret settings). Known Windows keychain limitation documented, with a `BALANZE_OPENAI_KEY` env-var fallback.
- **Distribution: source only.** `cargo install --git https://github.com/Oszkar/balanze balanze_cli` (the repo root is a virtual workspace, so the package is named explicitly; it builds the `balanze-cli` binary). No binaries, no installers, no GitHub Releases in v0.1 — the audience (org-admin tinkerer power-users) accepts the Rust-toolchain prerequisite. Linux works via `cargo install` (no separate test matrix; tray UI is later anyway).
- **Not in v0.1:** tray UI, popover, file watcher, pace view, alerts, dashboard window. All deliberately moved to later phases.

### Phase 2 — v0.2: Liveness

Make the data update itself, make the Anthropic API $ figure honest, and project forward. No UI yet; the CLI gets "alive." Delivery is sequenced into tracks so the riskiest work (a live loop) is preceded by the de-risking it depends on. Schema, new-crate, and secret-surface changes called out below pass through the `AGENTS.md` §8 change-control gate at implementation time.

**v0.1.1 — base (Track A; ships first, may fold into the v0.2 base).** Two correctness fixes the live loop depends on:

- **Proactive OAuth refresh.** Today the Anthropic OAuth bearer expires every ~7–8h and the CLI surfaces an `AuthExpired` error (re-run `claude login`). v0.1.1 refreshes the token *before* expiry (keepalive with a margin), not reactively on a 401 — a background watcher that silently goes stale every 8h is not "live." Touches the credentials secret surface (§3.4); the refresh mechanism (implement the refresh-token grant with atomic write-back vs. delegate to the `claude` CLI) is the one open design decision for the Track A plan.
- **Anchor the cap window to the server reset.** The rolling window is currently computed as `now − 5h`; v0.1.1 anchors it to the OAuth-reported `resets_at` (the authoritative reset the same endpoint already returns), removing clock-drift error from the cap math.

**Foundational de-risk (Track B; before any polling).** A single source-orchestration policy shared by the CLI and the future live loop, so the two composition paths cannot silently diverge; and the provider-politeness backoff (exponential, capped) the §3.1 API-politeness rule requires before any repeated polling exists. Track B shipped — composition policy extracted into the `snapshot_composer` crate (CLI ≡ future watcher, fixture-parity-tested) and a `backoff` exponential-retry layer added to both HTTP clients (CLI fail-fast; watcher will use the standard 30s×2ⁿ schedule).

**Anthropic API $ — honesty redesign (Track C; the headline product change).** v0.1's Anthropic API $ cell is a list-price figure recomputed from local JSONL × a vendored price table. v0.2 stops leading with that synthetic number. *(Track C as shipped hard-labeled the estimate in place. That is now **superseded by the R1 matrix reshape** — see "The usage matrix (presentation contract)": the estimate leaves the matrix cell entirely and becomes the separate "Subscription leverage" insight, so honesty is structural rather than label-dependent. The Track C work below — surfacing the real `extra_usage` overage and disproving the per-event-cost premise — stands unchanged.)*

- **Make the estimate honest + surface the real overage.** The earlier premise that "Claude Code records a per-event cost in the JSONL Balanze parses" is **false for current Claude Code** — verified absent across 790 real session files (2026-05-19); Anthropic removed `costUSD` from transcripts, which is why ccusage et al. recompute from tokens. The only Claude-provided *real* cost is the statusline session total — that is **Track D's** source, not this one. So Track C instead: (a) hard-labels the JSONL × list-price figure as "estimate / subscription leverage / NOT billed" so it can't read as real spend, and (b) surfaces the **real pay-as-you-go extra-usage overage** (next bullet). No new data source; rendering + docs only (no `Snapshot`/parsed-event schema change — `extra_usage` already rides in the snapshot).
- **Reconciliation spike on the OAuth `extra_usage` block — RESOLVED 2026-05-19.** The spike (`~/.gstack/projects/balanze/spike-extra-usage-reconciliation-20260519.md`) proved `extra_usage` raw ints are **cents** and the block is the claude.ai "Extra usage" pay-as-you-go **overage** meter — real money billed, exact, first-party (reconciled 3/3 against a Max-5x screenshot). It is promoted to a real overage line in CLI output (only when the user enabled it), explicitly distinct from the estimate. It is NOT "total spend this month" (no such $ surface exists for a Max user; Phase-0 admin spike already NO-GO).
- The true prepaid-credit **balance** (the remaining $ figure) has no documented per-user API — the Admin API is org-admin-gated (Phase-0 NO-GO) and the Console cookie-paste is fragile/unofficial (§3.3). With `extra_usage` now giving the real *overage* signal (Track C) and statusline giving authoritative quota + a session-cost estimate (Track D), the Console cookie-paste's marginal value no longer justifies its fragility: **demoted from a committed v0.3 item to "implement only if a real user need surfaces"** (the one thing still missing is the *current* prepaid balance — `extra_usage` already gives spent/limit — a minor gap, not worth a brittle scrape).

**Official statusline source (Track D; now the live backbone — promoted ahead of Track E).** **Verified 2026-05-19** (spike: `~/.gstack/projects/balanze/spike-statusline-payload-20260519.md`): Claude Code's `statusLine` command receives, on stdin, `rate_limits.{five_hour,seven_day}.{used_percentage,resets_at}` — the same server-authoritative window data `/api/oauth/usage` returns — plus `cost.total_cost_usd`, zero-auth, no rate limit, push-driven (per turn, 300 ms-debounced), local. Because OAuth `/api/oauth/usage` 429s Balanze per-account *exactly during active Claude Code use* (observed; recorded as a design-doc constraint), statusline — not OAuth — is the correct primary live signal. v0.2 adds a new schema-owning reader crate (§8; `claude_parser`-grade schema-drift discipline — the payload evolves, e.g. `context_window.*` changed at v2.1.132). **Caveats that make the model layered, not a replacement:** `rate_limits` is present only for Pro/Max, only after the first API response, and only while the user is in an active Claude Code session with Balanze wired as their `statusLine` command — so OAuth stays the cold-start / no-active-session fallback (best-effort, backoff'd, 429-tolerant) and local JSONL stays the always-available activity/estimate source. `cost.total_cost_usd` is a Claude-side *session estimate* ("may differ from your actual bill") — a third explicitly-labeled cost tier alongside the JSONL list-price estimate and the real `extra_usage` overage; none conflatable (Track C's honesty discipline, extended). **New scope:** Track D wires the `statusLine` config for the user (fold into `balanze-cli setup`, same pattern as the OpenAI-key wizard) — otherwise the source ships but nobody turns it on. Pairs with the `statusline` output mode (Track E). **Delivered 2026-05-20** (the `claude_statusline` crate + `balanze-cli statusline` + the `setup` wiring; verified against a real captured v2.1.144 payload). The live-Snapshot integration (statusline-push feeding the coordinator, OAuth demoted to fallback) is **Track E**'s redefined watcher — intentionally NOT in Track D (no `Snapshot`/`compose()` change shipped).

**Liveness features (Track E; the phase headline) — watcher redefined around the Track D spine.**

- Live spine = **statusline-push** (authoritative %+resets+session-cost when the user is active — no polling, no rate limit) **+ JSONL `notify`+debounce** (local activity / per-model estimate, always available). OAuth is demoted from the heartbeat to a **slow backoff'd cold/fallback poll** (`backoff::standard()` 30 s×2ⁿ) for when no statusline feed is arriving; it degrades gracefully (stale-with-warning), never tight-loops on 429. Simpler than the original notify+OAuth-poll design — the authoritative numbers arrive pushed.
- Predictive reset: EWMA over the rolling window with an explicit warm-up state machine (Insufficient → Uncertain → Confident) so the predictor never lies immediately after a window reset; output rode on `Snapshot.prediction`. **Superseded (2026-06-02): the predictor is being retired.** Dogfooding confirmed forecasting is the wrong mental model for a trust-first tool (predictions are inherently flaky; the warm-up machine existed only to make a flaky number less flaky). It is replaced by the **pace model** — measured *quota used %* vs *window elapsed %* plus a transparent used÷elapsed ratio, no forward forecast — per the "Show measured status, not forecasts" principle. See v0.3.0. The `predictor` crate is removed and the elapsed/pace computation folds into `window` (a `Snapshot` schema change, §8).
- `--watch` (long-running refresh loop) and the `statusline` output mode for shell prompts / status bars.
- Performance benchmarks (criterion) for the cost/parse hot paths land here, so the live refresh cadence has a measured budget and regressions are caught rather than guessed at.
- **Pre-v0.3 Sink-seam checkpoint.** Before Phase 3, exercise the `state_coordinator` `Sink` / future `TauriSink` boundary with a real consumer (the watcher feeding the coordinator). The Tauri side is still scaffold-only; this seam is the v0.2→v0.3 cliff and the #1 remaining roadmap risk — validate it here, not in v0.3.
- Integration robustness improvements informed by the first weeks of real v0.1 use. The watcher spawns `compose()` with a concrete `SnapshotSources` impl (Send inferred via static dispatch); a generic spawn-helper over `S: SnapshotSources` would need `trait_variant`/boxing to prove the future `Send`.

Sequencing: Track A → Track B → Track C → **Track D** → Track E. (Track D promoted ahead of E — verified 2026-05-19 as the live backbone the watcher is built around, not a parallel corroborator.) **Track E Delivered 2026-05-21** — the `predictor` + `watcher` crates, the `balanze-cli --watch` long-running mode (Stdout + JSONL sinks under a `tokio::select!` supervisor), the statusline-file IPC bridge between `balanze-cli statusline` and `watcher::tasks::statusline`, the compile-only `TauriSink` skeleton (the v0.2→v0.3 Sink-seam checkpoint), and committed Criterion baselines for the cost/parse hot paths.

### Phase 3 — v0.3: UI

The Tauri surface — the hero artifact of the project. The full UI scope from the original plan stays intact (popover, settings, alerts, dashboard), but it ships as **bounded sub-milestones**, each shippable on its own, so the popover screenshot exists early instead of after one large release. The biggest known risk — the `state_coordinator` `Sink` / `TauriSink` seam, validated as a compile-only skeleton in v0.2 — is exercised live in the very first sub-milestone.

**v0.3.0 — Popover (the hero).** The glanceable surface, and the thing that makes the whole backend legible.

- Tauri 2 popover/tray UI: color-shifting gauge tray icon (repaint deduped by `(ColorBucket, title_text)`), hidden-on-launch popover with one progress bar per Anthropic cadence + reset sublines, burn sparkline, the **pace view**, and the matrix tiles.
- **Pace view (replaces the retired predictor).** Per window (5h / 7-day), show two measured facts side by side — *quota used %* and *window elapsed %* — plus a transparent **pace ratio** (used ÷ elapsed) rendered as a glanceable verdict ("on pace" / "burning ~2.0× faster than linear"). It is pure division of two measured numbers, **not** a time forecast: always defined, no warm-up, no post-reset lie. The 30-minute burn sparkline (current rate, also a measured fact) stays alongside it.
- The tiles obey the **matrix presentation contract** (see Functional requirements): the 2×2 holds **measured reality only** — server quota % and real billed $ — with each cell carrying a **visible source/confidence badge** (REAL billed vs quota %), the primary quota source being the statusline feed with OAuth shown as the stale/fallback state. The **"Subscription leverage"** estimate (JSONL list-price) renders as a separate, clearly-secondary insight outside the grid — never as a matrix cell. Making the provenance model *visible* is a first-class goal of this milestone, not a label afterthought.
- Wires the live spine into the Tauri host (the watcher → coordinator → `TauriSink` path) and the minimal IPC the popover needs: `get_snapshot`, `get_history`, `refresh_now`, and the `usage_updated` event. `tauri-plugin-single-instance` so the user cannot double-launch.

**v0.3.1 — Settings & trust.**

- Settings UI: paste API keys, save to OS keychain. With a real key-input box the keychain code is exercised on both platforms, so this is where the **`keyring` → `keyring-core` (v4) migration** lands — fixing the v0.1 Windows keychain no-op, a visible flaw on the primary dev OS. Adds `set_api_key` / `get_settings` / `set_settings`.
- Surfaces the Track D `statusLine` wiring (the CLI `setup` does it headless; the settings UI shows/edits it).
- Degraded-state events surfaced visually (`degraded_state` event): stale data shown with a warning rather than blanked.

**v0.3.2 — Alerts.** Kept deliberately minimal — table-stakes, not gold-plated.

- OS notifications for: spend exceeds threshold, credits/quota below threshold, subscription approaching cap, reset window approaching, connector failure / stale data. Thresholds informed by v0.1–v0.2 observation, configured in the settings UI.

**v0.3.3 — Dashboard.**

- Optional full dashboard window: per-provider detail, recent trends/sparklines, source/confidence detail, troubleshooting.
- **Pulls in history persistence (SQLite).** Through v0.2 history is in-memory, rolling-window-sized. A trends view needs durable history, so the persistence layer (deferred since v0.1) lands here, as a dependency of the dashboard rather than speculative infrastructure.

**Demoted / not committed in v0.3:** the Anthropic Console cookie-paste (2026-05-19). Its only unique signal is the *current* prepaid balance; `extra_usage` (Track C) already gives the real overage spent/limit and statusline (Track D) gives authoritative quota — so the fragile/unofficial scrape (§3.3) is implemented only if a concrete user need surfaces. If ever built: cookie-paste UX, tile shows `auth_expired` and prompts a re-paste on 401.

### Phase 4 — v0.4: Distribution & Legibility

Make it runnable without a Rust toolchain, and make the engineering legible to anyone who looks. Deliberately **lightweight** (see Project intent) — the heavyweight signing/store work is optional, not load-bearing.

- **Runnable release.** Unsigned binaries on GitHub Releases (MSI/NSIS, DMG/app) so someone can download and run it without `cargo`. Linux still via `cargo install`.
- **Legibility.** A polished README with screenshots / a short GIF of the popover, and a "how it works" writeup centered on the three things worth showing: the data-provenance model (the measured-only matrix + the leverage insight), the "measured status, not forecasts" call (built an EWMA predictor, dogfooded it, retired it for honest pace facts), and the actor-model architecture / the twelve boundaries.
- "Send Logs" menu item bundling rotated logs + a recent state snapshot for support.
- **Optional (not committed):** Windows code-signing, macOS notarization, Homebrew tap, WinGet manifest, Tauri auto-update. Done only if the cert/admin cost feels worth it — low engineering-taste signal per hour, and unsigned-runnable already clears the "an evaluator can try it" bar. (If pursued, the release pipeline itself — notarization, auto-update manifest — is the artifact worth showing.)

### Vision — uncommitted

Genuinely uncommitted ideas, picked up as a bounded phase only if desire or a real need surfaces. Not promised, not sequenced — listed so the architecture keeps them cheap.

- **Prove the connector abstraction** — add a third provider (Gemini CLI / OpenRouter / Cursor) to turn the "one shared core, thin connectors" claim from asserted into demonstrated. The strongest single "this architecture generalizes" signal, if/when it's wanted.
- Ubuntu 24.04 LTS+ GNOME support — deferred until the Windows + macOS experience is solid (GNOME tray behavior is the most fragile of the three).
- Cross-device sync via a small relay (GitHub Gist / Cloudflare KV / iCloud Drive) — one Balanze identity reading/writing the same numbers across devices; sets up Android + hosted dashboard cleanly.
- Export / snapshot reporting; broader provider coverage as repeated personal need surfaces.
- Android companion app — read-only feed of the desktop's state via the sync layer.
- Hosted web dashboard for wallboard / TV use, reusing the normalized backend contracts.

## Open questions

Items still genuinely unresolved at the product level. The design doc carries the technical open questions and spike plan.

- Which provider metrics are realistically obtainable through stable official integration versus inference? The v0.1 Phase-0 spike resolved the Anthropic side at the API-availability level (official Usage & Cost API is enterprise/admin-gated → NO-GO for the modal user; JSONL-derived estimate shipped instead). v0.2's Track C narrows it further (RESOLVED 2026-05-19): the assumed per-event Claude cost does not exist in current JSONL (verified across 790 files), so Track C hard-labels the JSONL estimate as leverage-not-spend; and the OAuth `extra_usage` block was reconciled — it is the claude.ai pay-as-you-go overage meter (cents, exact, real billed money) and is surfaced as such, not "spend this month". The statusline source was verified 2026-05-19 (it carries authoritative 5h/7d %+resets plus a client-side session-cost estimate; promoted to the v0.2 live backbone — see Phase 2 Track D). The Anthropic Console cookie-paste (current prepaid balance) is demoted to "implement only if a real user need surfaces"; broader provider coverage in later phases needs similar per-provider investigation.
- Should onboarding ask users to choose a trust mode, such as "official only" versus "include estimated subscription metrics"? Deferred — v0.1 marks every metric with its source per the Transparency requirement, which may make an explicit trust-mode setting unnecessary.
- How much locally-stored history is needed before the product becomes meaningfully better than a simple current-status tool? v0.1–v0.2 keep a rolling-window-sized in-memory history. SQLite persistence is now scoped to **v0.3.3**, as a dependency of the dashboard's trends view (rather than speculative infrastructure) — the dashboard is the first surface that genuinely needs durable history.
- What is the right default alert threshold mix (alerts land in **v0.3.2**)? Decide after observing real-use patterns through v0.1–v0.3.1 — premature thresholds are noise.
- How should the popover present the matrix and the leverage insight visually? **Resolved at the model level** by the matrix presentation contract (measured-only grid + separate "Subscription leverage" insight, R1); the open part is purely the v0.3.0 visual treatment of the source/confidence badges, settled when the popover is designed in detail.
