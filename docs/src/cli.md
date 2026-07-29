# CLI Reference

`balanze-cli` is the headless surface and the reference composition the tray popover renders. It is a multi-command tool; bare `balanze-cli` with no subcommand defaults to `status`.

Every command accepts the [global flags](#global-flags). Run `balanze-cli help`, or `--help` on any subcommand, for the same reference at the terminal.

## `status`

The 4-quadrant compact status: Anthropic quota, OpenAI quota, and each provider's real billed API dollars, on a single screen. This is also what runs when you invoke `balanze-cli` with no subcommand at all.

| Flag | Effect |
|---|---|
| `--json` | Machine-readable JSON instead of the formatted view. Wins over `--sections` if both are given. |
| `--sections` | A per-source detailed view: cadence bars, model breakdown, the Codex rolling window. |

## `watch`

A live view that keeps refreshing in place instead of printing once and exiting: a `ratatui` TUI when stdout is a TTY, or a streaming line-per-update format otherwise (for example when piped to a file or another process).

| Flag | Effect |
|---|---|
| `--json` | Stream one JSON document per line instead of the live view. Implies non-interactive output even on a TTY. |

## `doctor`

Diagnoses each integration one at a time - Claude OAuth credential, Codex rollout files, the OpenAI keychain entry or environment variable - and prints an actionable hint next to anything that is not OK. This is the first thing to run when a cell looks wrong.

| Flag | Effect |
|---|---|
| `--offline` | Skip network validation of the OpenAI key (checks it is present and well-formed, but does not call the API to confirm it is accepted). |

## `export`

Exports usage history as CSV, re-derived statelessly from the local Claude JSONL and Codex rollout files each time it runs - there is no database to fall out of sync with.

| Flag | Effect |
|---|---|
| `-o`, `--output <PATH>` | Write to a file instead of stdout. |

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

Removes the OpenAI key from the keychain. Use this to rotate a key (clear, then `set-openai-key` with the replacement) or to stop Balanze from calling the OpenAI API entirely.

## `settings`

Prints the current contents of `settings.json` - useful for confirming what the desktop app or a previous `setup` run actually wrote, or for scripting a diff before and after a change.

## `statusline`

The Claude Code statusLine command. With no subcommand, this is the FROZEN stdin render contract that Claude Code itself invokes on every prompt render - its output format is a stable interface, not something to parse ad hoc. Wiring it into Claude Code's configuration happens through `setup` or the popover's settings panel, not by running `statusline` directly.

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
| `-h`, `--help` | Print help for the command. Add `-h` for a short summary or `--help` for the long form. |
| `-V`, `--version` | Print the CLI version. Top-level only; not repeated on each subcommand. |

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

A provider you simply have not configured is **not** an auth failure and does not exit 3. An absent credential is neutral - `status` exits 0 (or 5 under `--strict`), matching `doctor`, which warns rather than fails for one. Code 3 means a credential was found and the provider refused it (or it could not be read).

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
