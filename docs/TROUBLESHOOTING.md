# Developer troubleshooting

Non-obvious traps when working *on* Balanze. Entries here prescribe code and configuration changes, so they assume a checkout and a toolchain.

**Using Balanze rather than building it?** The user-facing answers are in the [FAQ](https://oszkar.github.io/balanze/faq.html).

## "Tray icon doesn't appear" or "two tray icons in the menu bar"

The double-tray-icon trap: `tauri.conf.json` declares a default tray with id `"main"`, and code in `lib.rs` creates a second tray via `TrayIconBuilder::new()`. The handler attaches to the invisible second icon; the visible one receives clicks that go nowhere.

Attach the handler to `app.tray_by_id("main")`, handling a missing tray explicitly. Reuse the existing tray rather than creating a second one; see `src-tauri/src/lib.rs`.

## "macOS tray click events don't fire"

If the handler is attached correctly (above) and clicks still don't fire on macOS, check `iconAsTemplate` in `tauri.conf.json`. Template-mode icons can interact strangely with click events on certain macOS versions. Balanze's tray icon should have `iconAsTemplate: false` (the color gauge IS the signal; we don't want macOS inverting it).

## "JSONL parser eats 100% CPU during an active Claude session"

The incremental-read cursor isn't working - the parser is doing a full re-parse on every notify event. Check `crates/claude_parser/`: on each watch event the parser should seek to the saved `byte_pos`, read to EOF, parse new lines only, then update the cursor. Initial ingestion and file replacement require full reads. `refresh_now()` only re-emits the current snapshot; it does not trigger parsing. Detect atomic replacements via platform file identity (device/inode on Unix, volume/file-index on Windows) combined with mtime/size - never file size alone - and detect a growing in-place rewrite via bounded probes of the committed prefix (see AGENTS.md §3.1).

## "Two app instances running simultaneously"

`tauri-plugin-single-instance` was either not registered, registered out of order, or its target attribute is wrong. The plugin must be registered **first** on the `tauri::Builder`, gated `#[cfg(any(target_os = "windows", target_os = "macos"))]`. The scaffold wires this correctly in `src-tauri/src/lib.rs::run`.

## "Tray icon flickers"

Check the supervised repaint task in `src-tauri`: icon, title, and tooltip are deduped against the properties actually accepted by the OS. Partial failures must not mark the whole target as painted. The tooltip can change even when the utilization bucket does not. See [architecture boundary 7](ARCHITECTURE.md) for the full contract.

## "`cargo check` fails after bumping a Tauri dep"

Check the resolved versions in `Cargo.lock` and the affected package's dependency requirements. Tauri core, build tooling, and plugins have independent version numbers; this workspace uses major-version `2` requirements, not one shared minor pin. Start with the actual macro/build error and `cargo tree --workspace --duplicates` before changing dependencies.

## "Frontend can't call my new Tauri command"

Check the `#[tauri::command]` declaration and its registration in `tauri::generate_handler![...]` first. For plugin or restricted APIs, also check the relevant capability permissions. Do not add broad capabilities as a workaround; new IPC and capability surfaces require the review in `AGENTS.md`.

## "Settings file got corrupted after a crash"

The `settings` crate must use both layers of its write contract: acquire the persistent sibling `settings.json.lock`, reload and apply field-level intent while holding it, then publish through the shared atomic-file helper. Direct writes can leave a truncated file after a crash; atomic rename without the surrounding transaction can still silently lose another process's independent change. Never lock `settings.json` itself because the atomic rename replaces that file identity.

## "balanze-cli statusline is wired but the Claude Code status line is blank (Windows)"

See the [user FAQ](https://oszkar.github.io/balanze/faq.html#the-statusline-is-wired-but-my-claude-code-prompt-is-blank-windows) for path escaping and PATH setup. To isolate rendering from Claude Code's invocation, pipe `crates/claude_statusline/tests/fixtures/real-payload.json` directly to `balanze-cli statusline`.

## "Cross-provider segments (Codex %, and OpenAI $ if you enabled it) appear in the statusline even when the desktop app / watcher is not running"

This is expected fallback behavior. See the [FAQ](https://oszkar.github.io/balanze/faq.html#codex-segments-appear-in-my-statusline-even-though-the-app-is-not-running) for user-facing behavior and [architecture](ARCHITECTURE.md#ipc-contract) for snapshot precedence and the shared OpenAI Costs gate. The default template omits `{openai_cost}`, so it performs no OpenAI fallback work.

## "`bun run tauri dev` hangs with `transport invoke timed out after 60000ms`"

Tracked in [#136](https://github.com/Oszkar/balanze/issues/136). The issue records a Vite module-runner timeout while loading SvelteKit's development runtime, including reproduction without Tauri. Its current diagnosis distinguishes cached timeout failures from the still-unexplained slow initial transform; do not assume a Windows-only deadlock or an IPv4 fix.

Use the issue for reproduction evidence and upstream-fix status. Production builds do not use the development module runner: `bun run tauri build --no-bundle` produces a binary for desktop smoke testing. The SvelteKit-free states gallery (`bun run gallery`) is useful for component checks but does not test Tauri integration.
