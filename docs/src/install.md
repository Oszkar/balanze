# Install

**Desktop app (tray popover):** download from [GitHub Releases](https://github.com/Oszkar/balanze/releases/latest) - no Rust toolchain required.

| Your machine | Download | First run |
|---|---|---|
| macOS 15+, Apple Silicon | `Balanze_<version>_aarch64.dmg`, or `brew install --cask oszkar/balanze/balanze` | Signed and notarized - Gatekeeper should not warn. |
| Windows 11, x64 | `Balanze_<version>_x64_en-US.msi` | Unsigned - SmartScreen warns once, see below. |

The `_x64-setup.exe` asset is the same Windows app as an NSIS installer instead of an MSI; pick either. `_aarch64.app.tar.gz` is the raw macOS app bundle for scripted installs - if you are not sure, take the DMG. The Homebrew cask is an alternative to the DMG for macOS users who would rather manage updates through `brew upgrade`; it installs the same signed, notarized app.

**Intel Macs are not supported.** The macOS build is Apple Silicon (arm64) only. macOS 15 already drops most Intel hardware, so a universal binary would double the build time and bundle size to serve machines that largely cannot run the required OS anyway. Building from source on an Intel Mac is untested but nothing blocks it. The desktop app is not built for Windows on arm64 either, though the CLI is - see [Command-line tool](#command-line-tool) below.

Windows installers are unsigned, so SmartScreen warns on first run: **"Windows protected your PC - Microsoft Defender SmartScreen prevented an unrecognized app from starting."**

This means Windows does not recognize the publisher. It does not mean the installer is malware. Balanze is unsigned because a code-signing certificate would not fix it: Microsoft no longer grants SmartScreen reputation for EV certificates, so a signed build from a project this size warns on first run too. [The PRD](https://github.com/Oszkar/balanze/blob/main/docs/PRD.md#code-signing) records the full reasoning.

Rather than ask you to trust a certificate, the offer is that you can check the download yourself. **Verify the checksum before clicking through the warning** - it is the step that actually tells you the file is what CI built, and it takes a few seconds. Then click **More info**, then **Run anyway**.

The other half of the offer is that the source is public and builds from scratch: see [Command-line tool](#command-line-tool) below for building the CLI, and `bun run tauri build` for the desktop app.

## Verifying a download

Every release attaches `windows-x64-checksums.txt` and `macos-aarch64-checksums.txt`. Worth doing on any download, and worth doing on the **unsigned Windows installers in particular**, since SmartScreen's warning is the only other signal you get.

Replace `<version>` with the release you downloaded, matching the filename exactly, and compare the output against the corresponding line in the checksums file:

```powershell
# Windows (PowerShell)
Get-FileHash .\Balanze_<version>_x64_en-US.msi -Algorithm SHA256
```

```bash
# macOS
shasum -a 256 Balanze_<version>_aarch64.dmg
```

## Command-line tool

`balanze-cli` is the full four-quadrant view in your terminal, the Claude Code statusline backend, and the entire Linux story.

| Platform | Install |
|---|---|
| macOS (Apple Silicon) | `brew install oszkar/balanze/balanze-cli` |
| Linux (x64) | `brew install oszkar/balanze/balanze-cli`, or download `balanze-cli-*-x86_64-unknown-linux-musl.tar.gz` |
| Windows (x64) | Download `balanze-cli-*-x86_64-pc-windows-msvc.zip` |
| Windows (arm64) | Download `balanze-cli-*-aarch64-pc-windows-msvc.zip` |

Direct downloads: extract the archive and put `balanze-cli` somewhere on your PATH. Every archive ships a sibling `.sha256` so you can verify it against what CI built.

The Linux binary is statically linked against musl, so it runs on any distribution regardless of glibc version. Linux has no OS credential store wired, so supply your OpenAI key through the `BALANZE_OPENAI_KEY` environment variable rather than `balanze-cli set-openai-key`.

On macOS, a browser-downloaded archive is quarantined by Gatekeeper because the CLI binary is not notarized. Homebrew installs are not quarantined, which is why it is the recommended path; for a direct download, run `xattr -d com.apple.quarantine balanze-cli` once.

**Building from source:** the CLI also compiles cleanly from source if you would rather not use a prebuilt binary. Requires Rust 1.89+.

```bash
# `--git` is required (not on crates.io). The repo root is a virtual workspace,
# so name the package explicitly - it builds the `balanze-cli` binary.
# Plain `cargo install balanze_cli` will NOT work.
cargo install --git https://github.com/Oszkar/balanze balanze_cli

balanze-cli setup      # run this first - wizard for the OpenAI admin key
balanze-cli            # 4-quadrant status
```

**The CLI has zero system-library dependencies.** Windows 11, macOS 15+, and Linux build with just the Rust toolchain (Linux also needs a C compiler for the `ring` TLS dependency). No GTK/GLib/Cairo/WebKit - that native stack belongs to the desktop app, not the CLI.

The Claude side needs no setup if Claude Code is already configured: Balanze reads its OAuth credential (from `~/.claude/.credentials.json`, `~/.config/claude/.credentials.json`, or Claude Code's login Keychain entry on recent macOS) strictly **read-only** - it never refreshes, modifies, or copies it. If it expires, re-run `claude login`. An opt-in file-refresh mode is tracked in [#186](https://github.com/Oszkar/balanze/issues/186). Provide the OpenAI Admin key via `balanze-cli setup`, `set-openai-key`, the popover's settings panel, or the `BALANZE_OPENAI_KEY` env var.

> **macOS note:** builds you compile yourself are unsigned, so macOS can't reliably remember a Keychain "Always Allow" grant across rebuilds - expect the occasional repeat password prompt for the Claude Code credential and/or a saved OpenAI key. The downloadable release DMG is signed and notarized (from v0.5.0 onward) and does not have this problem.

For a full walkthrough - first run, reading the popover, connecting OpenAI, wiring the statusline - see the [User Guide](GUIDE.md).
