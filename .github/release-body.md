Auto-generated release.

**Desktop app**

- **Windows (x64):** `Balanze_*_x64_en-US.msi`, or `Balanze_*_x64-setup.exe` for the NSIS installer. Unsigned - SmartScreen will warn on first run; click "More info" -> "Run anyway". [Why we don't sign](https://github.com/Oszkar/balanze/blob/main/docs/PRD.md#code-signing).
- **macOS 15+ (Apple Silicon):** `Balanze_*_aarch64.dmg`. Signed and notarized - Gatekeeper should not warn. Intel Macs are not supported.

**Command-line tool**

- **Homebrew (macOS and Linux):** `brew install oszkar/balanze/balanze-cli`.
- **Direct download:** `balanze-cli-*-x86_64-pc-windows-msvc.zip`, `balanze-cli-*-aarch64-pc-windows-msvc.zip`, `balanze-cli-*-aarch64-apple-darwin.tar.gz`, or `balanze-cli-*-x86_64-unknown-linux-musl.tar.gz` (static, runs on any Linux). Extract and put `balanze-cli` on your PATH.
- The macOS CLI archive is unsigned. A browser download is quarantined by Gatekeeper; either install via Homebrew, which is not, or run `xattr -d com.apple.quarantine balanze-cli` once.

**Verify a download** against the `*-checksums.txt` files or an archive's sibling `.sha256`.
