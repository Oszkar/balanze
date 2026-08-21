# CLI Reference

`balanze-cli` is the headless surface and the reference composition the tray popover renders. It is a multi-command tool; bare `balanze-cli` with no subcommand defaults to `status`.

Every command accepts the [global flags](#global-flags). Run `balanze-cli help`, or `--help` on any subcommand, for the same reference at the terminal.

## `status`

The 4-quadrant compact status: Anthropic quota, OpenAI quota, and each provider's real billed API dollars, on a single screen. This is also what runs when you invoke `balanze-cli` with no subcommand at all. OpenAI Costs is shared with every other Balanze process through an exact 300-second gate; a recent failed or in-flight attempt can therefore defer that source instead of starting another request.

| Flag | Effect |
|---|---|
| `--json` | Machine-readable JSON instead of the formatted view. Wins over `--sections` if both are given. |
| `--sections` | A per-source detailed view: cadence bars, model breakdown, the Codex rolling window. |

On a terminal it renders like this:

```text
=== Balanze status (2026-05-20 04:27:42 UTC) ===

                    Quota %                                 API $ (real billed)
Anthropic           ok 82% 5h, 88% 7d (oauth)               $20.92/$25.00 overage (real)
OpenAI              ok 6% 7d (codex go)                     $4.20 (admin costs)

Pace: 5h 82% used / 60% elapsed (1.4x);  7d 88% used / 95% elapsed (0.9x)
Subscription leverage: ~$2197.11 of this month's Claude Code usage at API list prices (leverage - NOT billed)
```

Without pay-as-you-go "Extra usage" enabled on claude.ai, the Anthropic API-$ cell reads `- not available` instead, and only the leverage line carries a Claude dollar figure. That is deliberate: Anthropic exposes no per-user API spend, and the cell is never backfilled with a substitute number.

### The `--json` schema

`status --json` (and `watch --json`) currently emit schema version 2. Version 2 adds the nullable `claude_oauth_unavailable: string | null` field so a consumer can distinguish "Claude Code not installed" from a cold start. All version 1 fields keep their existing names and types, but consumers that reject unknown schema versions must explicitly accept version 2 before upgrading.

Every money cell is tagged `{ value_micro_usd, source, confidence, details }` in i64 micro-USD - so a consumer can tell an estimate from real billed spend straight from the wire shape, without parsing labels. The full schema is documented in [`docs/ARCHITECTURE.md`](https://github.com/Oszkar/balanze/blob/main/docs/ARCHITECTURE.md).

## `watch`

A live view that keeps refreshing in place instead of printing once and exiting: a `ratatui` TUI when stdout is a TTY, or a streaming line-per-update format otherwise (for example when piped to a file or another process).

| Flag | Effect |
|---|---|
| `--json` | Stream one JSON document per line instead of the live view. Implies non-interactive output even on a TTY. |

## `doctor`

Diagnoses each integration one at a time - Claude OAuth credential, Codex rollout files, the OpenAI keychain entry or environment variable - and prints an actionable hint next to anything that is not OK. This is the first thing to run when a cell looks wrong.

| Flag | Effect |
|---|---|
| `--offline` | Skip the network check on the OpenAI key. This is a **presence** check only: any non-empty value passes, so a truncated or malformed key still reports OK. Drop `--offline` to have the key actually tried against the API. |

## `export`

Exports usage history as CSV, re-derived statelessly on every run - nothing is persisted. The output carries two provenance-segregated sections: Claude usage from the local JSONL (one row per day and model, with token counts plus a list-price *leverage* figure that is never money billed) and OpenAI current-month real billed spend per line item from the Admin Costs API. The OpenAI section needs a configured OpenAI key and either a reusable current-month full result in the shared 300-second gate or permission from that gate to make one network request; the Claude section does not.

| Flag | Effect |
|---|---|
| `-o`, `--output <OUTPUT>` | Write to a file instead of stdout. |

## `completions`

Prints a shell completion script to stdout for the named shell. See [Shell completions](#shell-completions) below for how to install the output.

| Argument | Values |
|---|---|
| `<SHELL>` | `bash`, `zsh`, `fish`, `powershell`, `elvish` |

## `setup`

The interactive setup wizard. Walks through storing the OpenAI Admin key and offers to wire the Claude Code statusline, backing up any command it replaces first. This is the recommended first command to run after installing the CLI.

## `set-openai-key`

Prompts for an OpenAI Admin key (masked input) and stores it in the OS keychain. On Linux, where no OS credential store is wired, this command cannot save anything and tells you so - use the `BALANZE_OPENAI_KEY` environment variable instead.

## `clear-openai-key`

Removes the OpenAI key from the keychain. To rotate a key on Windows or macOS, clear it and then `set-openai-key` with the replacement.

Two things this does **not** do:

- **It does not override `BALANZE_OPENAI_KEY`.** That variable takes precedence over the keychain, so if it is set, Balanze keeps calling the OpenAI API with it after the keychain entry is gone. Unset it as well to actually stop the calls or to change which key is used.
- **It is not the rotation path on Linux**, where no OS credential store is wired and `set-openai-key` has nothing to save to. Change the environment variable instead.

## `settings`

Prints the current contents of `settings.json` - useful for confirming what the desktop app or a previous `setup` run actually wrote, or for scripting a diff before and after a change.

## `statusline`

The Claude Code statusLine command. With no subcommand, this is the FROZEN stdin render contract that Claude Code itself invokes on every prompt render - its output format is a stable interface, not something to parse ad hoc. Wiring it into Claude Code's configuration happens through `setup` or the popover's settings panel, not by running `statusline` directly.

It needs no credentials of its own. The default line carries live 5-hour and 7-day Claude subscription quota, an estimate of the current session's cost, and cross-provider signal in the form of both Codex rate-limit windows.

Real OpenAI API spend is available as a `{openai_cost}` segment but is **off by default**, because it is an uncapped dollar figure with no rolling window - it reads oddly next to a line that is otherwise percent-of-window, and it is the only segment that can ask the shared OpenAI Costs gate for an API call. Adding it to your configured line switches the OpenAI leg on. During a deferred refresh, statusline can show the last headline with a stale marker; it does not need the full per-line-item result used by `export`.

### `statusline restore`

Restores the foreign statusLine command that Balanze replaced when it was wired in - or unwires Balanze's own statusline if there was nothing to restore. Safe to run even if you are unsure whether Balanze ever replaced anything.

## `man`

Prints the man page in roff format to stdout. Hidden from `--help` because it exists for packagers rather than for interactive use, but documented here so it is a decision rather than a secret.

```bash
balanze-cli man > balanze-cli.1
```

## Global flags

These apply to every subcommand, including the bare default (`balanze-cli` with no subcommand, which runs `status`).

| Flag | Effect |
|---|---|
| `-v`, `--verbose` | Surface account-identifying fields (org uuid, Codex `session_id`) that are omitted by default. |
| `--quiet` | Suppress non-essential output. |
| `--no-color` | Disable ANSI color. `NO_COLOR` is also honored - see [Environment variables](#environment-variables). |
| `--strict` | Treat a degraded source as failure: a stale or errored source that would otherwise exit 0 exits 5 instead. See [Exit codes](#exit-codes). |
| `-h`, `--help` | Print help for the command. At the top level, `-h` gives a short summary and `--help` the long form; on every subcommand, both just print help. |

`-V` / `--version` prints the CLI version, but it is **top-level only**: `balanze-cli --version` works, `balanze-cli status --version` does not.

## Exit codes

`main` classifies the outcome once, and `doctor` shares the same taxonomy.

| Code | Meaning |
|------|---------|
| 0 | OK (a degraded source still exits 0 unless `--strict`) |
| 1 | unexpected / other error |
| 2 | usage error (bad flags / unknown subcommand; clap owns this) |
| 3 | auth: credentials expired or rejected (re-run `claude login`, or refresh the OpenAI key) |
| 4 | network: a provider was unreachable |
| 5 | degraded: a source was stale or errored (only with `--strict`) |

A provider you simply have not configured is **not** an auth failure and does not exit 3. Code 3 means a credential was found and the provider refused it, or it could not be read.

`status` and `doctor` treat an absent credential differently under `--strict`, which matters if you script against them:

- **`status`** classifies only populated error slots, and an unconfigured provider populates none. It exits **0**, with or without `--strict`. Nothing about a provider you never set up can produce exit 5.
- **`doctor`** reports the same situation as a warning, and `--strict` folds warnings into exit **5**.

So `doctor --strict` is the one to use if you want a missing credential to fail a script; `status --strict` will not do it.

## Environment variables

| Variable | Effect |
|---|---|
| `BALANZE_OPENAI_KEY` | An OpenAI Admin key. Takes precedence over the OS keychain on every platform, which makes it the escape hatch when a keychain is locked, and the only supported route on Linux. |
| `BALANZE_LOG` | Log verbosity, same syntax as `RUST_LOG` (for example `BALANZE_LOG=debug`). Applies to both the CLI and the desktop app. An invalid value warns and falls back to `info`. |
| `NO_COLOR` | Honored alongside `--no-color`. |

Logs go to stderr and to a daily-rotating file kept for three days, under the OS-conventional data directory's `logs/` folder.

## Shell completions

`balanze-cli completions <shell>` prints a script to stdout (bash, zsh, fish, powershell, elvish):

```bash
balanze-cli completions bash > ~/.local/share/bash-completion/completions/balanze-cli
balanze-cli completions zsh  > "${fpath[1]}/_balanze-cli"
balanze-cli completions fish > ~/.config/fish/completions/balanze-cli.fish
```

For a walkthrough of what these commands look like in practice, see the [User Guide](GUIDE.md). If something still looks wrong, the [FAQ](faq.md) covers the questions that come up most.
