<p align="center">
  <img src="assets/logo.svg" alt="" width="120">
</p>

# Balanze

Balanze is a local-first tray utility that consolidates personal AI usage into one normalized view: Anthropic subscription quota, OpenAI Codex quota, real OpenAI API spend, and an estimate of what your Claude Code usage would cost at API prices.

Everything in the headline view is **measured reality only** - a server-reported quota percentage or a real billed dollar amount - so a column never mixes kinds of numbers.

|               | Quota %                              | API $ (real billed)                                 |
|---------------|--------------------------------------|-----------------------------------------------------|
| **Anthropic** | OAuth usage (5h / 7-day / per-model) | `extra_usage` overage if you enabled it, else *n/a* |
| **OpenAI**    | Codex CLI rate-limit % (5h / weekly) | real billed spend (Admin Costs API)                 |

The Claude list-price figure is deliberately **not** in that grid. It sits outside as a separate *Subscription leverage* insight, so a counterfactual estimate can never be mistaken for billed spend.

## Get Balanze

**Desktop tray app** - Windows 11 (x64) and macOS 15+ (Apple Silicon). Download and run it; no Rust toolchain needed. See [Install](install.md).

**Command line** - Windows, macOS, and Linux. `balanze-cli` is the full view in your terminal, the Claude Code statusline backend, and the entire Linux story. See [Install](install.md#command-line-tool).

## Read next

- **[User Guide](GUIDE.md)** - first run, reading the popover, connecting OpenAI, the statusline.
- **[CLI Reference](cli.md)** - every `balanze-cli` command and flag, exit codes, and environment variables.
- **[FAQ & Troubleshooting](faq.md)** - common questions, answered.

Source, releases, and issues live in the [GitHub repository](https://github.com/Oszkar/balanze).
