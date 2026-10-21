Balanze v0.5.4 - Window consistency & maintenance

This release shows Codex credits, makes quota window labels consistent, refreshes Claude pricing, and adds a maintainer price-refresh command. Stale quota keeps its utilization color in CLI status, the tray uses a distinct violet degraded ring, and `watch` shares compact status's pace colors. See the [full changelog](https://github.com/Oszkar/balanze/blob/v0.5.4/CHANGELOG.md) for details.

**What's included**

- Codex credits appear in both popover layouts and `status --sections`, with zero or absent balances hidden. `status --json` exposes the nullable credits object and preserves reported zero balances. Credits reflect the last Codex session. No new credits segment is added to `watch` or statusline.
- Window labels retain model-specific Anthropic limits and identify unknown Codex durations honestly. Codex windows remain plan-dependent: weekly-only plans do not gain a synthetic five-hour value.
- Bundled Claude pricing includes Opus 5.5, Sonnet 5.5, and Haiku 5.5, including Haiku's above-100,000-prompt-token tier. Historical prices remain available for exports.
- Maintainers can preview a pinned LiteLLM price refresh with `just refresh-claude-prices <full-sha> --dry-run`. Official-price cross-checking remains manual; runtime pricing stays offline.
- rustls 0.23.46 addresses four TLS advisories; Tauri and other dependencies are updated.

**Before upgrading**

- Upgrade every running copy, including the desktop app and any `balanze-cli` on your PATH. The machine-wide OpenAI request and settings-write guarantees require cooperating upgraded processes.
- CLI JSON output now uses **schema version 3**, adding nullable `codex_quota.credits` with `has_credits`, `unlimited`, and `balance` as whole credits. All version 2 fields retain their names and types; consumers that reject unknown schema versions must accept version 3 before upgrading.
- When upgrading from before v0.5.3, also account for schema 2's nullable `claude_oauth_unavailable` field and CSV export's 13th `partial` column. A partial export warns even with `--quiet` and exits 5 with `--strict`; authentication and network failures use exit codes 3 and 4.

**Desktop app**

- **Windows (x64):** `Balanze_*_x64_en-US.msi`, or `Balanze_*_x64-setup.exe` for the NSIS installer. Unsigned - SmartScreen will warn on first run; click "More info" -> "Run anyway". [Why we don't sign](https://github.com/Oszkar/balanze/blob/main/docs/PRD.md#code-signing).
- **macOS 15+ (Apple Silicon):** `brew install --cask oszkar/balanze/balanze`, or download `Balanze_*_aarch64.dmg`. Signed and notarized - Gatekeeper should not warn. Intel Macs are not supported.

**Command-line tool**

- **Homebrew (macOS and Linux):** `brew install oszkar/balanze/balanze-cli`.
- **Direct download:** `balanze-cli-*-x86_64-pc-windows-msvc.zip`, `balanze-cli-*-aarch64-pc-windows-msvc.zip`, `balanze-cli-*-aarch64-apple-darwin.tar.gz`, or `balanze-cli-*-x86_64-unknown-linux-musl.tar.gz` (static, runs on any Linux). Extract and put `balanze-cli` on your PATH.
- The macOS CLI archive is unsigned. A browser download is quarantined by Gatekeeper; either install via Homebrew, which is not, or run `xattr -d com.apple.quarantine balanze-cli` once.

**Verify a download** against the `*-checksums.txt` files (installers) or an archive's sibling `.sha256` (CLI). The raw `*.app.tar.gz` bundle is not checksummed - take the DMG if you want a verifiable macOS download.
