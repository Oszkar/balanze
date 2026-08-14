# Developer troubleshooting

Non-obvious traps when working *on* Balanze. Entries here prescribe code and configuration changes, so they assume a checkout and a toolchain.

**Using Balanze rather than building it?** The user-facing answers are in the [FAQ](https://oszkar.github.io/balanze/faq.html).

## "Tray icon doesn't appear" or "two tray icons in the menu bar"

The double-tray-icon trap: `tauri.conf.json` declares a default tray with id `"main"`, and code in `lib.rs` creates a second tray via `TrayIconBuilder::new()`. The handler attaches to the invisible second icon; the visible one receives clicks that go nowhere.

Fix: attach the handler via `app.tray_by_id("main").unwrap().on_tray_icon_event(...)`, never via `TrayIconBuilder::new()`. The scaffold already does this correctly in `src-tauri/src/lib.rs`; don't refactor it back.

## "macOS tray click events don't fire"

If the handler is attached correctly (above) and clicks still don't fire on macOS, check `iconAsTemplate` in `tauri.conf.json`. Template-mode icons can interact strangely with click events on certain macOS versions. Balanze's tray icon should have `iconAsTemplate: false` (the color gauge IS the signal; we don't want macOS inverting it).

## "JSONL parser eats 100% CPU during an active Claude session"

The incremental-read cursor isn't working - the parser is doing a full re-parse on every notify event. Check `crates/claude_parser/`: on each watch event the parser should seek to the saved `byte_pos`, read to EOF, parse new lines only, then update the cursor. Full reparse happens only on launch and on explicit `refresh_now()`. Detect atomic replacements via platform file identity (device/inode on Unix, volume/file-index on Windows) combined with mtime/size - never file size alone - and detect a growing in-place rewrite via bounded probes of the committed prefix (see AGENTS.md §3.1).

## "Two app instances running simultaneously"

`tauri-plugin-single-instance` was either not registered, registered out of order, or its target attribute is wrong. The plugin must be registered **first** on the `tauri::Builder`, gated `#[cfg(any(target_os = "windows", target_os = "macos"))]`. The scaffold wires this correctly in `src-tauri/src/lib.rs::run`.

## "Tray icon flickers"

Tray repaint isn't deduped. The coordinator notifies the `Sink` on every snapshot update (and on a `StateMsg::Refresh` from popover-open / `refresh_now`); the production `TauriSink` should only call `tray.set_icon`/`set_title`/`set_tooltip` when the `(ColorBucket, title, tooltip)` tuple differs from its `last_painted` - the tooltip is part of the key because it names the worst window, so a same-color/same-title repaint that only changed the tooltip must still paint. If you see flicker during idle, that dedup check is missing or comparing the wrong fields.

## "`cargo check` fails after bumping a Tauri dep"

`tauri`, `tauri-build`, and `tauri-plugin-*` must all share the same minor version. Mixed minors (e.g. `tauri 2.11` + `tauri-build 2.6`) cause cryptic `generate_context!` macro errors. The workspace `Cargo.toml` pins these together via `workspace.dependencies`; if you bump one, bump them all in lockstep.

## "Frontend can't call my new Tauri command"

The command needs three things wired: (1) function declared `#[tauri::command]`, (2) listed in `tauri::generate_handler![...]` inside `run()`, (3) capability declared in `src-tauri/capabilities/default.json` (for any non-default API). Forgetting any of these gives the same opaque error. Check `default.json` and the `generate_handler!` block first.

## "Settings file got corrupted after a crash"

The `settings` crate must use both layers of its write contract: acquire the persistent sibling `settings.json.lock`, reload and apply field-level intent while holding it, then publish through the shared atomic-file helper. Direct writes can leave a truncated file after a crash; atomic rename without the surrounding transaction can still silently lose another process's independent change. Never lock `settings.json` itself because the atomic rename replaces that file identity.

## "If the Anthropic Console scrape ever lands and breaks overnight"

Not a live entry: the Console cookie-paste scrape is **not implemented** (`DataSource::AnthropicConsoleScrape` exists in `claude_parser` as a reserved variant only), and it is opt-in-if-ever per AGENTS.md §3.3. Kept here because the guidance is the point: Console UI changes would break a scrape regularly, so it would be best-effort by design - mark the data stale via the source's `*_error` slot / `degraded_state` event and tell the user. Don't try to "make the scrape more robust" by spending a week on it; if the official endpoint isn't there, that's the answer.

## "balanze-cli statusline is wired but the Claude Code status line is blank (Windows)"

Almost always the `statusLine.command` path in `~/.claude/settings.json` uses single backslashes. Two things mangle it at once: JSON parses `\b` / `\t` / `\r` as control characters (so `...\balanze\target\release...` decodes to backspace / tab / carriage-return garbage), and Claude Code runs the status line through Git Bash on Windows, where backslashes are escape characters. Both fail silently - the mangled command isn't found, so the line is just empty (no error surfaces).

Fix: use forward slashes, which are valid in Windows file APIs, JSON, and Git Bash all at once: `"command": "e:/Programming/balanze/target/release/balanze-cli.exe statusline"`. To prove the binary itself is fine, pipe a payload straight to it: `balanze-cli statusline < some-payload.json` (try `crates/claude_statusline/tests/fixtures/real-payload.json`). Once `balanze-cli` is on `PATH` (after distribution), the bare `balanze-cli statusline` invocation avoids absolute-path escaping entirely.

## "Cross-provider segments (Codex %, and OpenAI $ if you enabled it) appear in the statusline even when the desktop app / watcher is not running"

Expected behavior - not a bug. `balanze-cli statusline` self-composes these segments when no fresh `snapshot.json` exists: Codex is read directly from local files, and OpenAI cost - **if** a configured line contains `{openai_cost}` - is obtained through the shared OpenAI Costs gate at `<cache>/statusline/openai-cost.json`. The same gate covers `status`, `export`, the watcher, and key validation, so all Balanze processes together can start at most one request for a request identity per exact 300-second reservation. If a fetch fails, the last known headline is served to statusline with a `⚠️` marker; full-data callers receive a deferred result until the reservation expires. With the default template the OpenAI segment is off, and statusline performs no OpenAI key, cache, lease, or API work. Starting the desktop app or `balanze-cli watch` produces a fresh `snapshot.json` which takes precedence for statusline rendering.

The gate is deliberately strict across UTC month rollover. If an attempt starts just before the first day of a month, `status`, `export`, watcher updates, and key validation can be deferred for the remaining part of its 300-second reservation even though a new direct request would target the new month. Statusline may show the prior-month headline as stale during that window. Retry after the reservation expires. The cache isolates different keys and API base URLs, holds at most 8 identities, and never stores either plaintext value. OpenAI organization selection is not supported; the Admin key's organization determines the Costs data.

## "`bun run tauri dev` hangs with `transport invoke timed out after 60000ms`"

**Open, unresolved** - tracked in [#136](https://github.com/Oszkar/balanze/issues/136). Vite 8's module runner deadlocks on Windows while evaluating SvelteKit's server runtime, so the dev server accepts the connection but never serves a page. The Rust side is unaffected: the app compiles and the tray appears, only the webview stays blank.

Two mitigations are already in place and are **not** sufficient on Vite 8.1.3: binding the Vite server to `127.0.0.1` (`vite.config.ts`) with a matching `devUrl` in `src-tauri/tauri.conf.json`. Forcing IPv4 resolution (`NODE_OPTIONS=--dns-result-order=ipv4first`) does not help either. The deadlock reproduces deterministically with `bun run dev` alone on a warm cache, so the original "Node resolves `localhost` to `::1` first" root cause does not explain the current failure. See #136 for the evidence and candidate next steps.

Workaround: this is the **dev server only** - production builds embed the frontend and never invoke the module runner. `bun run tauri build --no-bundle` then running `target/release/balanze.exe` works, as does the states gallery (`bun run gallery`), which is SvelteKit-free.
