# Common dev commands for the balanze repo.
# Use `just <recipe>` from the repo root.
#
# PowerShell 7+ is required on Windows so recipes work the same on every
# shell (cmd, bash, zsh, PowerShell 7+).

set windows-shell := ["pwsh.exe", "-NoLogo", "-NoProfile", "-Command"]

default:
    @just --list

# Lint gate - rustfmt + clippy -D warnings + svelte-check + cargo deny.
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    bun run check
    cargo deny check

# Fix Rust formatting in place.
fmt:
    cargo fmt --all

# Run the full test suite (Rust via cargo-nextest, frontend via vitest).
test:
    cargo nextest run --workspace
    bun run test

# Run the desktop app with frontend hot-reload.
dev:
    bun run tauri dev

# Release build (MSI/NSIS on Windows, DMG/app on macOS).
build:
    bun run tauri build
