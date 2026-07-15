# Balanze user guide

A walkthrough of the desktop app and the CLI: first run, reading the popover, connecting OpenAI, the Claude Code statusline, and the states you might run into.

New here? Start with the [README](../README.md) for what Balanze is and how to install it. This guide picks up after install.

> **Screenshots.** Lines marked `📷 [capture: <state>]` are placeholders. Every `<state>` is a real entry in the dev-only states gallery - run `bun run gallery` (or `bun run gallery:snap` for Playwright captures), screenshot the named state in light **and** dark, save to `docs/assets/guide/`, and replace the placeholder line with `![alt](assets/guide/<file>.png)`.

## First run

1. Install the CLI (`cargo install --git https://github.com/Oszkar/balanze balanze_cli`) or launch the desktop app.
2. Run `balanze-cli setup`. The wizard walks through the OpenAI Admin key and offers to wire the Claude Code statusline.
3. The Claude side needs no setup if Claude Code is already configured - Balanze reads its OAuth credential directly.

On the desktop app, first launch auto-opens the popover and fires a notification so the tray icon is easy to find.

> 📷 [capture: Settings - configured] the settings panel after setup, keys in place.

## Reading the popover

The popover is one normalized snapshot of your AI usage. Everything in the grid is **measured reality only** - a server-reported quota % or a real billed dollar amount - so a column never mixes kinds of numbers.

> 📷 [capture: Grid - two providers] the default grid, Anthropic and OpenAI side by side.

### The matrix

|               | Quota %                              | API $ (real billed)                                 |
|---------------|--------------------------------------|-----------------------------------------------------|
| **Anthropic** | OAuth usage (5h / 7-day / per-model) | `extra_usage` overage if you enabled it, else *n/a* |
| **OpenAI**    | Codex CLI rate-limit % (5h / weekly) | real billed spend (Admin Costs API)                 |

- **Anthropic quota %** - live 5-hour / 7-day utilization from the same `/api/oauth/usage` endpoint Claude Code uses, with a reset clock on each bar.
- **OpenAI quota %** - the Codex CLI rate-limit % for both rolling windows (5-hour + weekly, classified by duration), read from your local Codex rollout files.
- **OpenAI API $** - this-month billed spend from the Admin Costs API.
- **Anthropic API $** - real or nothing. If you enabled pay-as-you-go "Extra usage" on claude.ai, this cell shows that real overage; otherwise it reads **not available** (Anthropic exposes no per-user API spend). It is never backfilled with an estimate.

> 📷 [capture: Grid - overage billed] the Anthropic billed cell showing a real overage amount.

### Subscription leverage (a separate estimate)

Below the grid, the **Subscription leverage** box shows what your Claude Code usage *would* cost at API list prices (local JSONL times a vendored price table). For Pro/Max users this is leverage from the subscription, **never billed** - so it sits outside the matrix, where it can't be mistaken for spend.

### Pace and burn

- **Pace** rides on the usage bar: how much of a window you have used versus how far through the window you are. Over 1.0x means you are ahead of pace. Balanze shows measured pace, not a forecast.
- **Burn** is the recent token rate for the active Claude session.

### Source and confidence

Cells carry a badge for real billed money so you can tell it apart from an estimate at a glance. Hover any cell for its source and confidence.

### Grid vs Cards

A density toggle switches between the compact grid and a Cards view with the same data and more room per provider.

> 📷 [capture: Cards - two providers] the Cards density view.

## The tray icon

The tray gauge is a color-shifting ring on one shared scale - **green / yellow / orange / red at 50 / 75 / 90** - used identically across the tray, popover, CLI, and statusline. The ring colors on your **worst** window, and the title and tooltip name which window that is, so the color is always explained by a number you can see. Before there is any data the gauge is neutral, and the tooltip reads "connecting..." while a source warms up or "... unavailable" when one is not configured.

The design record behind this color language is in [`reviews/surface-consistency.html`](reviews/surface-consistency.html).

## Connecting OpenAI

OpenAI spend and Codex quota need an OpenAI **Admin** key (`sk-admin-...`), created in your OpenAI org's API-key settings. A regular `sk-...` key will not reach the Admin Costs API.

Provide it any of these ways:

- `balanze-cli setup` or `balanze-cli set-openai-key` (masked prompt).
- The popover's settings panel (Set / Replace / Remove). The key is validated before it is saved, and stored in your OS keychain - never written to disk in plaintext.
- The `BALANZE_OPENAI_KEY` env var (handy for CI or a locked keychain; takes precedence over the keychain).

Until a key is present, the OpenAI column shows a connect prompt rather than a blank cell.

> 📷 [capture: Grid - OpenAI connect CTA] the "add OpenAI" affordance before a key is set.

## The Claude Code statusline

`balanze-cli statusline` is a zero-auth status line for your Claude Code prompt: live 5h/7d subscription quota, session cost, and - uniquely - cross-provider signal (both Codex rate-limit windows, 5h and 7d) in one line. Real OpenAI API spend is available as an opt-in `{openai_cost}` segment, off by default.

- **Wire it** during `balanze-cli setup`, or from the popover's settings panel.
- **Replace, don't wrap.** If another tool already owns the `statusLine.command`, Balanze offers to replace it *with your consent*, backing the previous command up first. Nothing in the other tool's own config is touched.
- **Restore** the previous command at any time with `balanze-cli statusline restore` (or the settings panel).

## States you might see

Balanze names each situation instead of blanking a cell:

- **Cold start** - a source is still connecting.
  > 📷 [capture: Grid - cold start (quota loading)]
- **Claude Code not detected** - a neutral "not configured" state, not an error (the tray stays neutral, no warning).
  > 📷 [capture: Grid - Claude Code not detected]
- **Stale window** - e.g. a Codex window that already reset degrades to a `stale` marker rather than showing a confidently-wrong number.
  > 📷 [capture: Grid - Codex stale window]
- **Fetch error** - a failed source shows an error placeholder and raises the degraded-state banner naming the affected source.
  > 📷 [capture: Grid - OpenAI error]

## The CLI in brief

The CLI renders the same snapshot headlessly:

- `balanze-cli` - the compact 4-quadrant status (colored on a TTY).
- `balanze-cli watch` - a live TUI (streams JSON when piped or given `--json`).
- `balanze-cli doctor` - per-integration diagnostics with actionable hints.
- `balanze-cli export -o usage.csv` - a stateless CSV re-derived from JSONL.

Run `balanze-cli help` (or `--help` on any subcommand) for the full reference, and see the [README](../README.md#using-the-cli) for the exit-code taxonomy and JSON schema.

## Settings

The popover's gear opens settings:

- **Keys** - set / replace / remove the OpenAI Admin key.
- **Provider toggles** - enable or disable each provider live; a disabled provider's cell clears instead of going stale.
- **Statusline** - wire, unwire, or restore the Claude Code statusline.

## Troubleshooting

If something looks wrong, `balanze-cli doctor` diagnoses each integration with a hint per source. The non-obvious traps (double tray icons, JSONL CPU spikes, a stale statusline, macOS Keychain prompts) are collected in [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md).
