# How it works

Two decisions shape everything Balanze shows you: what counts as data, and who is allowed to touch it.

## Measured, not estimated

Every cell in the popover's grid is **measured reality** - a server-reported quota % or a real billed dollar amount - never a guess. That rule keeps a column honest: you can never mistake an estimate for something you'll actually be billed for.

|               | Quota %                              | API $ (real billed)                                 |
|---------------|---------------------------------------|------------------------------------------------------|
| **Anthropic** | OAuth usage (5h / 7-day / per-model) | `extra_usage` overage if enabled, else *n/a*         |
| **OpenAI**    | Codex CLI rate-limit % (5h / weekly) | real billed spend (Admin Costs API)                  |

Anthropic exposes no per-user API spend, so that cell is real or nothing - never backfilled with a substitute number.

One number deliberately sits *outside* this grid: **Subscription leverage**. `claude_cost` multiplies your local Claude Code JSONL by a vendored LiteLLM price table to show what that usage would cost at API list prices. For Pro/Max users, that's leverage from the subscription - genuinely useful context, but never billed, so it can't be mistaken for spend.

## Measured status, not forecasts

Balanze shows pace - how much of a rate-limit window you've used versus how far through the window you are - instead of predicting when you'll run out. An earlier version built and dogfooded an EWMA-based usage predictor; it was retired in favor of the honest pace figure, because a plausible-looking forecast that's occasionally wrong is worse than a fact that's always right.

## One thread owns the truth

Balanze is an actor-model app: a single `state_coordinator` task owns the canonical `Snapshot`, pollers for each provider feed it updates, and the tray, popover, and CLI only ever read from it. Nothing else writes state, issues HTTP requests, touches secrets, or renders the on-disk JSONL/statusline formats - each of those is the strict responsibility of exactly one crate.

That layering (twelve boundaries in total) is what lets Balanze add a new provider as an isolated connector crate without touching the rest of the system. The full list, plus the crate map and data-flow diagram, is in [`ARCHITECTURE.md`](https://github.com/Oszkar/balanze/blob/main/docs/ARCHITECTURE.md).
