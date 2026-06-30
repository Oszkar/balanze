## "Tray icon doesn't appear" or "two tray icons in the menu bar"

The double-tray-icon trap: `tauri.conf.json` declares a default tray with id `"main"`, and code in `lib.rs` creates a second tray via `TrayIconBuilder::new()`. The handler attaches to the invisible second icon; the visible one receives clicks that go nowhere.

Fix: attach the handler via `app.tray_by_id("main").unwrap().on_tray_icon_event(...)`, never via `TrayIconBuilder::new()`. The scaffold already does this correctly in `src-tauri/src/lib.rs`; don't refactor it back.

## "macOS tray click events don't fire"

If the handler is attached correctly (above) and clicks still don't fire on macOS, check `iconAsTemplate` in `tauri.conf.json`. Template-mode icons can interact strangely with click events on certain macOS versions. Balanze's tray icon should have `iconAsTemplate: false` (the color gauge IS the signal; we don't want macOS inverting it).

## "JSONL parser eats 100% CPU during an active Claude session"

The incremental-read cursor isn't working — the parser is doing a full re-parse on every notify event. Check `crates/claude_parser/`: on each watch event the parser should seek to the saved `byte_pos`, read to EOF, parse new lines only, then update the cursor. Full reparse happens only on launch and on explicit `refresh_now()`. Detect atomic rewrites via `(current.size, current.mtime)` vs the stored cursor — never just file size.

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

Expected. Console UI changes will break scrapes regularly — that's why the design defers this to v0.3 (now opt-in) and treats it as best-effort. Mark the data stale via `DegradedState::parse_error` and inform the user. Don't try to "make the scrape more robust" by spending a week on it; if the official endpoint isn't there, that's the answer.

## "balanze-cli statusline is wired but the Claude Code status line is blank (Windows)"

Almost always the `statusLine.command` path in `~/.claude/settings.json` uses single backslashes. Two things mangle it at once: JSON parses `\b` / `\t` / `\r` as control characters (so `...\balanze\target\release...` decodes to backspace / tab / carriage-return garbage), and Claude Code runs the status line through Git Bash on Windows, where backslashes are escape characters. Both fail silently - the mangled command isn't found, so the line is just empty (no error surfaces).

Fix: use forward slashes, which are valid in Windows file APIs, JSON, and Git Bash all at once: `"command": "e:/Programming/balanze/target/release/balanze-cli.exe statusline"`. To prove the binary itself is fine, pipe a payload straight to it: `balanze-cli statusline < some-payload.json` (try `crates/claude_statusline/tests/fixtures/real-payload.json`). Once `balanze-cli` is on `PATH` (after distribution), the bare `balanze-cli statusline` invocation avoids absolute-path escaping entirely.

## "`bun run tauri dev` hangs with `transport invoke timed out after 60000ms`"

A Vite 8 module-runner stall while SvelteKit's dev server evaluates its server runtime module (`@sveltejs/kit/.../server/index.js`), almost always right after clearing `.vite` / `.svelte-kit` forces a cold dependency re-optimization. It is a frontend dev-server cold-start problem, not app code: the Rust side builds and launches fine, and the app is SPA (`src/routes/+layout.ts` sets `ssr = false`), so this is the module-runner transport, not real SSR.

Fixes, in order: (1) just retry - the failed run warms `.vite`, so the second run usually succeeds (kill any lingering `vite` / `balanze.exe` holding port 1420 first); (2) isolate with `bun run dev` alone and open <http://localhost:1420> to tell Vite/SvelteKit apart from the Tauri webview; (3) clean reinstall: `Remove-Item -Recurse -Force node_modules, .svelte-kit; bun install`; (4) `bun run tauri build --no-bundle` compiles the frontend with `vite build` (no dev module-runner) and runs the produced binary, sidestepping the hang for a one-off desktop smoke. Avoid pre-clearing `.vite` unless you need to - clearing it is what forces the slow cold optimize that times out.