\# Balanze



\## Overview



Balanze is a local-first desktop utility for tracking personal AI usage across multiple providers in one place. The initial target is a tray-first desktop app built with Tauri 2 and Rust for Windows 11, macOS 15+ and Ubuntu 24.04 LTS+ on GNOME, with a future path to Android and a hosted web dashboard.



The product goal is simple: reduce the friction of checking multiple tools, tabs, billing pages and account surfaces to answer a few recurring questions — How much of Claude has been used today? How much OpenAI API credit remains? When does a limit reset? How close is the current account to budget or cap?



This is a side project, so the product should optimize for usefulness, low maintenance, and tight scope rather than maximum provider coverage on day one.



\## Problem



Heavy AI users increasingly split work across consumer subscriptions and API accounts. A single person may use ChatGPT for interactive work, Claude for coding or long-form reasoning, and one or more API accounts for automation, while each platform exposes usage, billing, credits and limits in different ways.



Today, the available tooling is fragmented. Existing open-source tools are often Claude-specific, CLI-only, IDE-bound, or focused on developer observability rather than personal cross-provider usage monitoring.



This creates three user problems:

\- Time wasted checking multiple sources manually.

\- Poor awareness of reset windows, credit depletion, and spend trends.

\- No single normalized view across subscription usage and API usage.



\## Users



\### Primary user



An individual power user, developer, founder, product manager, or researcher who actively uses more than one AI platform and wants lightweight visibility without running a SaaS admin product.



\### Secondary user



A small team or technically inclined individual who wants a local dashboard and alerting layer for personal or shared accounts, but not full enterprise observability.



\## Goals



\### Product goals



\- Provide one desktop app that shows AI usage across multiple providers in a normalized way.

\- Support both subscription-style usage and API billing or credit usage where feasible.

\- Make the app useful from the tray or menu bar with minimal interaction.

\- Keep all core functionality local-first and transparent.

\- Preserve a clean path to later add Android and a hosted dashboard without rewriting the core architecture.



\### MVP goals



\- Ship on Windows 11, macOS 15+ and Ubuntu 24.04 LTS+ GNOME only.

\- Support at least OpenAI and Anthropic as the first two providers.

\- Show current usage snapshot, reset timing where available, spend or credits where available, and lightweight recent history.

\- Allow threshold alerts for spend, quota, or estimated remaining usage.



\### Non-goals



\- Full enterprise cost allocation or multi-seat observability.

\- Native OS widgets in the initial roadmap.

\- Broad Linux desktop support beyond Ubuntu GNOME.

\- Browser automation or brittle scraping as a headline feature.

\- Monetization, subscriptions, cloud sync, or team billing in the first version.



\## Product principles



\- Local-first by default.

\- Honest about data quality and source provenance.

\- Fast glanceability before deep dashboards.

\- One shared core with thin platform shells.

\- Narrow support matrix over broad but unreliable compatibility.



\## Scope



\### In scope for MVP



\- Desktop application using Tauri 2 + Rust.

\- Tray or menu-bar presence with popup and optional full dashboard window.

\- Provider connectors for OpenAI and Anthropic.

\- Unified account list across subscription and API account types.

\- Manual refresh plus periodic background refresh.

\- Local secure storage for credentials and preferences.

\- Simple historical trend storage for recent usage.

\- Alerts for thresholds and reset windows.



\### Out of scope for MVP



\- Android app.

\- Hosted dashboard.

\- Collaboration features.

\- Team analytics and seat breakdowns.

\- Provider marketplace with dozens of integrations.

\- Native widgets.



\## Supported platforms



| Platform | Support level | Notes |

|---|---|---|

| Windows 11 | Supported at launch | Desktop tray-first experience. Windows 10 excluded. |

| macOS 15+ | Supported at launch | Menu-bar-first experience with optional dashboard window. |

| Ubuntu 24.04 LTS+ GNOME | Supported at launch | GNOME only, with AppIndicator tray support in the tested environment. |

| Android | Future | Companion app only after desktop MVP proves value. |

| Hosted web dashboard | Future | Separate surface for wallboard or TV use, reusing normalized backend contracts. |



\## Key use cases



\- See all major AI accounts and balances in one glance.

\- Check Claude subscription usage and reset timing without opening the web app, where reliable data is available.

\- Check OpenAI API spend and remaining credits quickly.

\- Get notified when nearing a usage limit or spending threshold.

\- Compare current billing-cycle usage across providers.

\- Open a deeper dashboard for trends and account details.



\## Core user experience



\### Main modes



The app should support three presentation modes from the same codebase:

\- Tray or menu-bar only.

\- Tray or menu-bar with compact popup.

\- Full dashboard window, optionally hidden until opened.



Users should be able to configure startup behavior, refresh cadence, alert thresholds and preferred compact metrics.



\### Information hierarchy



The compact view should prioritize:

1\. Provider status.

2\. Current usage or spend.

3\. Remaining credits or estimated remaining quota.

4\. Reset time or billing-cycle progress.

5\. Alerts.



The full dashboard should add:

\- Per-provider detail pages.

\- Recent trends.

\- Account source and confidence indicators.

\- Settings, connectors and troubleshooting.



\## Functional requirements



\### Provider model



The system should normalize data into a common schema:

\- Provider name.

\- Account type: subscription, API, team, or other.

\- Metrics: spend, credits remaining, usage consumed, reset time, billing-cycle window, request or token counts where available.

\- Data source type: official API, official dashboard, imported file, manual input, inferred.

\- Confidence level: exact, partial, estimated.



\### Integrations



\#### OpenAI



The product should support API usage and billing-related visibility using official surfaces where available, including usage and dashboard concepts already documented by OpenAI.



\#### Anthropic



The product should support Claude-related usage views across API billing and subscription usage where reliable access is available. Claude documents subscription usage and length limits as dynamic and plan-dependent, so the app should present these carefully and avoid overstating precision.



\### Alerts



Users should be able to define alerts for:

\- Spend exceeds threshold.

\- Credits remaining below threshold.

\- Subscription usage nearing cap.

\- Reset window approaching.

\- Connector failure or stale data.



\### Storage



\- Store credentials locally using OS-appropriate secure storage when possible.

\- Store settings and recent history locally.

\- No mandatory cloud backend.



\### Transparency



Every provider metric should expose its source and precision. For example, an exact API credit balance should not be displayed in the same way as an estimated subscription remaining value.



\## UX requirements



\- The compact popup should open fast and be glanceable in under 5 seconds.

\- The tray or menu-bar state should surface the single most useful live indicator, such as total spend today, provider warning count, or next reset.

\- Visual design should remain functional and lightweight rather than dashboard-heavy.

\- Users should understand at a glance what is official data versus inferred data.



\## Technical approach



\### Architecture



Recommended architecture:

\- Rust core for data normalization, polling, storage, alerting, and provider connector logic.

\- Tauri desktop shell for tray behavior, windows, OS integration and packaging.

\- Shared frontend for popup and dashboard views.

\- Connector abstraction layer per provider so future Android or hosted surfaces can reuse the same normalized model.



\### Design implications



This architecture keeps the desktop product as the first-class surface while making future web or Android clients consumers of the same normalized domain model, rather than forks of the business logic.



\## Risks and constraints



\### Main risks



\- Provider data access may be inconsistent across API usage versus subscription usage.

\- Subscription tracking may depend on surfaces that are not designed as public integrations.

\- Linux tray behavior remains more fragile than Windows or macOS even with a constrained GNOME-only support policy.

\- Side-project time constraints increase the importance of strict scope control.



\### Mitigations



\- Launch with only a small set of high-value integrations.

\- Mark all metrics with source provenance and confidence.

\- Prefer official endpoints and documented surfaces first.

\- Treat unsupported subscription data as optional, not foundational.

\- Keep the platform matrix narrow and explicit.



\## Competitive landscape



Current open-source tools validate demand but do not fully cover the proposed product shape. Existing examples include Claude-specific taskbar or menu-bar apps, local usage monitors, and CLI analyzers such as CodeZeno's Claude Code Usage Monitor, IgniteStudiosLtd's claude-usage-tool, usage-monitor-for-claude, and ccusage.



The main market gap appears to be a local-first, open-source, multi-provider desktop utility that combines subscription visibility and API billing or credit visibility in one normalized experience.



\## Success criteria



For an MVP side project, success should be measured qualitatively and with a few lightweight quantitative checks:

\- The app becomes the default place to check AI usage daily.

\- Two providers work reliably enough for personal use.

\- The tray or compact popup answers the core status question in a few seconds.

\- Setup time for one provider is under 10 minutes.

\- Alerting reduces surprise credit depletion or quota exhaustion.



\## Phasing



\### Phase 1



\- Desktop MVP.

\- OpenAI and Anthropic support.

\- Tray or menu-bar compact UI.

\- Full dashboard window.

\- Local alerts and history.



\### Phase 2



\- Improve integration robustness.

\- Add more providers if there is repeated personal need.

\- Add export or snapshot reporting.



\### Phase 3



\- Hosted dashboard for wallboard or TV view.

\- Android companion app, if the product proves sticky enough to justify mobile effort.



\## Open questions



\- Which provider metrics are realistically obtainable through stable official integration versus inference?

\- Should the first release optimize primarily for API users, subscription users, or a balanced mix?

\- What is the single best tray-level metric to show by default?

\- Should onboarding ask users to choose a trust mode, such as official only versus include estimated subscription metrics?

\- How much history is needed locally before the product becomes meaningfully better than a simple current-status tool?

