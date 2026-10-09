Balanze v0.5.3 - Hardening

This release fixes usage undercounting, one-hour cache-write pricing, stale quota displays, concurrent settings updates, and duplicate OpenAI Costs requests. See the [full changelog](https://github.com/Oszkar/balanze/blob/v0.5.3/CHANGELOG.md#053---hardening---2026-10-09) for details.

**Before upgrading**

- Upgrade every running copy, including the desktop app and any `balanze-cli` on your PATH. The machine-wide OpenAI request and settings-write guarantees require cooperating upgraded processes.
- CLI JSON output now uses **schema version 2**, adding the nullable `claude_oauth_unavailable` field. All version 1 fields retain their names and types, but consumers that reject unknown schema versions must accept version 2 before upgrading.
- CSV export adds a 13th **`partial`** column: `true` or `false` for OpenAI rows, empty for Claude rows. Update consumers that require an exact header or column count. Existing column positions are unchanged.
- A partial OpenAI export warns on stderr even with `--quiet` and exits 5 with `--strict`. Authentication and network failures now use the documented exit codes 3 and 4.

**Desktop app**

- **Windows (x64):** `Balanze_*_x64_en-US.msi`, or `Balanze_*_x64-setup.exe` for the NSIS installer. Unsigned - SmartScreen will warn on first run; click "More info" -> "Run anyway". [Why we don't sign](https://github.com/Oszkar/balanze/blob/main/docs/PRD.md#code-signing).
- **macOS 15+ (Apple Silicon):** `brew install --cask oszkar/balanze/balanze`, or download `Balanze_*_aarch64.dmg`. Signed and notarized - Gatekeeper should not warn. Intel Macs are not supported.

**Command-line tool**

- **Homebrew (macOS and Linux):** `brew install oszkar/balanze/balanze-cli`.
- **Direct download:** `balanze-cli-*-x86_64-pc-windows-msvc.zip`, `balanze-cli-*-aarch64-pc-windows-msvc.zip`, `balanze-cli-*-aarch64-apple-darwin.tar.gz`, or `balanze-cli-*-x86_64-unknown-linux-musl.tar.gz` (static, runs on any Linux). Extract and put `balanze-cli` on your PATH.
- The macOS CLI archive is unsigned. A browser download is quarantined by Gatekeeper; either install via Homebrew, which is not, or run `xattr -d com.apple.quarantine balanze-cli` once.

**Verify a download** against the `*-checksums.txt` files (installers) or an archive's sibling `.sha256` (CLI). The raw `*.app.tar.gz` bundle is not checksummed - take the DMG if you want a verifiable macOS download.
