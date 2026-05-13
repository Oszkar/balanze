# Balanze

Local-first desktop utility for tracking personal AI usage across multiple providers in one place. Tray-first, built with Tauri 2 + Rust + Svelte 5. Targets Windows 11 and macOS 15+.

## Status

Pre-v0.1 — scaffolding only. See `docs/prd.md` for the product spec.

## Stack

- **Tauri 2** desktop shell, system tray, packaging
- **Rust** core (workspace at `crates/` will hold parser / window / predictor / openai_client / state_coordinator / watcher / keychain / settings)
- **Svelte 5 + TypeScript + Vite** popover and settings UI (SvelteKit with `adapter-static`, SPA mode)

## Quick start (dev)

Prerequisites: Rust 1.77+, Bun 1.3+, platform build tools (Windows: WebView2 + VS Build Tools; macOS: Xcode CLI).

```bash
bun install
bun run tauri dev
```

The app launches with a tray icon. The main window starts hidden — click the tray icon (or "Open Balanze" in the tray menu) to show it.

## Build (release)

```bash
bun run tauri build
```

Bundles land in `src-tauri/target/release/bundle/`:

- Windows: `.msi` and `.exe` (NSIS)
- macOS: `.dmg` and `.app`

CI builds for both OSes on tag `v*.*.*` — see `.github/workflows/release.yml`.

## Layout

```
balanze/
├── Cargo.toml              workspace root
├── package.json            bun + Svelte
├── src/                    Svelte frontend (popover + settings UI)
├── src-tauri/              Tauri app crate (single-instance + tray)
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── icons/
│   └── src/lib.rs
├── crates/                 Rust workspace members land here as the build progresses
├── docs/prd.md             product requirements
└── .github/workflows/      CI + release pipelines
```

## License

MIT — see `LICENSE`.
