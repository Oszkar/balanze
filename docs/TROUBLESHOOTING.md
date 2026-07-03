## "Tray icon doesn't appear" or "two tray icons in the menu bar"

The double-tray-icon trap: `tauri.conf.json` declares a default tray with id `"main"`, and code in `lib.rs` creates a second tray via `TrayIconBuilder::new()`. The handler attaches to the invisible second icon; the visible one receives clicks that go nowhere.

Fix: attach the handler via `app.tray_by_id("main").unwrap().on_tray_icon_event(...)`, never via `TrayIconBuilder::new()`. The scaffold already does this correctly in `src-tauri/src/lib.rs`; don't refactor it back.

## "macOS tray click events don't fire"

If the handler is attached correctly (above) and clicks still don't fire on macOS, check `iconAsTemplate` in `tauri.conf.json`. Template-mode icons can interact strangely with click events on certain macOS versions. Balanze's tray icon should have `iconAsTemplate: false` (the color gauge IS the signal; we don't want macOS inverting it).

## "JSONL parser eats 100% CPU during an active Claude session"

The incremental-read cursor isn't working - the parser is doing a full re-parse on every notify event. Check `crates/claude_parser/`: on each watch event the parser should seek to the saved `byte_pos`, read to EOF, parse new lines only, then update the cursor. Full reparse happens only on launch and on explicit `refresh_now()`. Detect atomic rewrites via `(current.size, current.mtime)` vs the stored cursor - never just file size.

## "Two app instances running simultaneously"

`tauri-plugin-single-instance` was either not registered, registered out of order, or its target attribute is wrong. The plugin must be registered **first** on the `tauri::Builder`, gated `#[cfg(any(target_os = "windows", target_os = "macos"))]`. The scaffold wires this correctly in `src-tauri/src/lib.rs::run`.

## "Tray icon flickers"

Tray repaint isn't deduped. The coordinator notifies the `Sink` on every snapshot update (and on a `StateMsg::Refresh` from popover-open / `refresh_now`); the production `TauriSink` should only call `tray.set_icon`/`tray.set_title` when the `(ColorBucket, title_text)` tuple differs from its `last_painted`. If you see flicker during idle, that dedup check is missing or comparing the wrong fields.

## "`cargo check` fails after bumping a Tauri dep"

`tauri`, `tauri-build`, and `tauri-plugin-*` must all share the same minor version. Mixed minors (e.g. `tauri 2.11` + `tauri-build 2.6`) cause cryptic `generate_context!` macro errors. The workspace `Cargo.toml` pins these together via `workspace.dependencies`; if you bump one, bump them all in lockstep.

## "Frontend can't call my new Tauri command"

The command needs three things wired: (1) function declared `#[tauri::command]`, (2) listed in `tauri::generate_handler![...]` inside `run()`, (3) capability declared in `src-tauri/capabilities/default.json` (for any non-default API). Forgetting any of these gives the same opaque error. Check `default.json` and the `generate_handler!` block first.

## "Settings file got corrupted after a crash"

The `settings` crate must use the atomic-write pattern: write to `settings.json.tmp`, then `rename` over `settings.json`. Direct writes truncate the existing file before writing new content; a crash mid-write leaves it empty. If you see this, the atomic-write pattern was bypassed.

## "Anthropic Console scrape stopped working overnight"

Expected. Console UI changes will break scrapes regularly - that's why the design defers this to v0.3 (now opt-in) and treats it as best-effort. Mark the data stale via `DegradedState::parse_error` and inform the user. Don't try to "make the scrape more robust" by spending a week on it; if the official endpoint isn't there, that's the answer.

## "balanze-cli statusline is wired but the Claude Code status line is blank (Windows)"

Almost always the `statusLine.command` path in `~/.claude/settings.json` uses single backslashes. Two things mangle it at once: JSON parses `\b` / `\t` / `\r` as control characters (so `...\balanze\target\release...` decodes to backspace / tab / carriage-return garbage), and Claude Code runs the status line through Git Bash on Windows, where backslashes are escape characters. Both fail silently - the mangled command isn't found, so the line is just empty (no error surfaces).

Fix: use forward slashes, which are valid in Windows file APIs, JSON, and Git Bash all at once: `"command": "e:/Programming/balanze/target/release/balanze-cli.exe statusline"`. To prove the binary itself is fine, pipe a payload straight to it: `balanze-cli statusline < some-payload.json` (try `crates/claude_statusline/tests/fixtures/real-payload.json`). Once `balanze-cli` is on `PATH` (after distribution), the bare `balanze-cli statusline` invocation avoids absolute-path escaping entirely.

## "Cross-provider segments (Codex %, OpenAI $) appear in the statusline even when the desktop app / watcher is not running"

Expected behavior - not a bug. Since PR3, `balanze-cli statusline` self-composes these segments when no fresh `snapshot.json` exists: Codex is read directly from local files, and OpenAI cost is fetched from the Admin Costs API and cached in `<cache>/statusline/openai-cost.json` for up to 5 minutes. At most one upstream OpenAI request fires per 300 seconds across all concurrent turns (the §3.1 politeness gate). If a fetch fails, the last known value is served with a `⚠` marker rather than blanking the segment; the endpoint is not retried for 60 seconds. Starting the desktop app or `balanze-cli watch` produces a fresh `snapshot.json` which takes precedence and the self-compose path is bypassed entirely.

## "`bun run tauri dev` hangs with `transport invoke timed out after 60000ms`"

Vite 8's module-runner `fetchModule` RPC (the `vite:invoke` transport between the dev-SSR runner and the main Vite process) deadlocks while SvelteKit's dev server loads its server runtime (`@sveltejs/kit/.../server/index.js`). It is PERSISTENT (not a cold-cache hiccup - it reproduces on warm runs) and is a frontend-toolchain problem, not app code: the Rust side builds and launches fine, and the app is SPA (`src/routes/+layout.ts` sets `ssr = false`), so this is the module-runner transport, not real SSR. The pinned `vite@8.0.12` is rolldown-vite (it depends on `rolldown`), whose module-runner is new, so the deadlock is most likely a rolldown-vite issue on Windows. Likely latent since the Vite 8 bump because the v0.4.x cycle was CLI-only. Tracked in [#136](https://github.com/Oszkar/balanze/issues/136).

Until it is fixed, work around it: (1) `bun run dev` alone + a browser at <http://localhost:1420> for UI work (this also tells Vite/SvelteKit apart from the Tauri webview); (2) `bun run tauri build --no-bundle` compiles the frontend with `vite build` (no dev module-runner) and produces a runnable binary - use it for desktop smokes. The real fix is a toolchain change (candidates in #136: downgrade to Vite 7 + `@sveltejs/vite-plugin-svelte` 6, or a newer rolldown-vite patch). Clearing `.vite` / reinstalling does NOT help and forces a slow cold re-optimize on top.