# Statusline Glyph Grammar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the Claude Code statusline a single glyph rule (`<emoji><space><content>`, emoji names the provider), relabel the Codex weekly window `7d` for cross-surface parity, and drop the OpenAI API spend segment from the default line without leaving its fetch machinery running.

**Architecture:** Five sequential tasks, each leaving the tree green. Glyph changes are confined to `statusline_render/src/render.rs` (the glyphs are hard-coded there, not in the template, so they reach every user regardless of a persisted `settings.json`). The default-template change is deliberately sequenced **last** among the code tasks, because dropping `{openai_cost}` breaks the self-compose integration test until the config-dir override and the demand gate exist to support it.

**Tech Stack:** Rust 2024, `cargo nextest`, `wiremock`, `assert_cmd`, `tempfile`.

Spec: `docs/superpowers/specs/2026-07-14-statusline-glyph-grammar-design.md`

## Global Constraints

- **No em-dashes.** Use regular hyphens (`-`) everywhere, including Rust doc comments, code comments, docs, and commit messages (AGENTS.md §3.5).
- **No Unicode ellipsis.** Use three periods (`...`) (AGENTS.md §3.5).
- **No project-management jargon** in commit messages, PR titles, or code comments. No task IDs, phase labels, or version tags in code comments; durable spec cross-references like `AGENTS.md §3.1` are fine (AGENTS.md §8).
- **Conventional Commits**, enforced by a blocking `commit-msg` hook: `<type>(scope)?(!)?: subject`.
- **Never `--no-verify`, never `--no-gpg-sign`.** If a hook fails, fix the cause.
- **Lint floor:** `cargo clippy --workspace --all-targets -- -D warnings` must pass. No `#[allow]` without a documented reason in a comment immediately above.
- **Validation gate for every Rust task** (AGENTS.md §6):
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo nextest run --workspace
  ```
- **Exact glyphs.** Copy these literally; the variation selectors are load-bearing:
  - `🧠` U+1F9E0 - context
  - `💰` U+1F4B0 - session cost estimate
  - `🤖` U+1F916 - model
  - `✳️` U+2733 U+FE0F - Claude (starburst mark)
  - `🌀` U+1F300 - Codex / OpenAI
  - `⚠️` U+26A0 U+FE0F - stale marker. The **VS16 (U+FE0F) is required**; today's bare `⚠` renders one cell wide instead of two, which is part of the defect being fixed.

---

### Task 1: Glyph grammar in the renderer

Every segment becomes `<emoji><space><content>`. The Codex weekly window is relabeled `wk` -> `7d`, matching Claude's nomenclature and `crates/balanze_cli/src/render.rs:1127`, which already asserts that surface must call it `7d`.

**Files:**
- Modify: `crates/statusline_render/src/render.rs` (segment rendering + its `#[cfg(test)] mod tests`)
- Modify: `crates/balanze_cli/src/statusline.rs` (one existing unit test pins the old glyphs)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: the rendered glyph vocabulary that Task 4's integration assertions rely on. In particular the `openai_cost` segment renders as `🌀 $4.20` (no `OpenAI` literal), and the Codex windows render as `🌀 5h {n}%` and `🌀 7d {n}%`.

- [ ] **Step 1: Write the failing tests**

Append these five tests inside the existing `#[cfg(test)] mod tests` block at the bottom of `crates/statusline_render/src/render.rs`. The existing `cfg()`, `now()`, and `snap()` helpers are already in that module.

```rust
    /// A full-config render exercising every segment's glyph at once. This is
    /// the glyph table: provider glyph for rate windows, metric glyph for the
    /// Claude-only figures, exactly one space after each.
    #[test]
    fn glyph_table() {
        let mut c = cfg();
        c.lines = vec!["{model} {context_bar} {cost} {usage} {codex} {openai_cost}".to_string()];
        let s = snap();
        let cross = CrossProvider {
            codex_five_hour: Some(12.0),
            codex_weekly: Some(7.0),
            openai_cost_micro_usd: Some(0),
            ..Default::default()
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: Some(&cross),
            config: &c,
            now: now(),
            color: false,
        });
        assert!(out.contains("🤖 Opus"), "model: {out}");
        // snap() has context 42% and the default bar width is 10 -> 4 filled cells.
        assert!(out.contains("🧠 [####------] 42%"), "context: {out}");
        assert!(out.contains("💰 ~$2.50"), "cost: {out}");
        assert!(out.contains("✳️ 5h 82%"), "claude 5h: {out}");
        assert!(out.contains("✳️ 7d 88%"), "claude 7d: {out}");
        assert!(out.contains("🌀 5h 12%"), "codex 5h: {out}");
        assert!(out.contains("🌀 7d 7%"), "codex weekly labeled 7d: {out}");
        assert!(out.contains("🌀 $0.00"), "openai cost: {out}");
    }

    /// The Codex weekly window must read "7d", never "wk". Every other surface
    /// in the repo already calls it 7d (see balanze_cli/src/render.rs), and the
    /// provider glyph is what distinguishes it from Claude's 7d window.
    #[test]
    fn codex_weekly_is_labeled_7d_not_wk() {
        let mut c = cfg();
        c.lines = vec!["{codex}".to_string()];
        let s = snap();
        let cross = CrossProvider {
            codex_weekly: Some(7.0),
            ..Default::default()
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: Some(&cross),
            config: &c,
            now: now(),
            color: false,
        });
        assert!(out.contains("🌀 7d 7%"), "{out}");
        assert!(!out.contains("wk"), "the 'wk' label is retired: {out}");
    }

    /// The one spacing rule: every glyph is followed by exactly one space, and
    /// the line never contains a double space.
    #[test]
    fn every_glyph_is_followed_by_exactly_one_space() {
        let mut c = cfg();
        c.lines = vec!["{model} {context_bar} {cost} {usage} {codex} {openai_cost}".to_string()];
        let s = snap();
        let cross = CrossProvider {
            codex_five_hour: Some(12.0),
            codex_weekly: Some(7.0),
            openai_cost_micro_usd: Some(0),
            ..Default::default()
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: Some(&cross),
            config: &c,
            now: now(),
            color: false,
        });
        for glyph in ["🤖", "🧠", "💰", "✳️", "🌀"] {
            for (idx, _) in out.match_indices(glyph) {
                let rest = &out[idx + glyph.len()..];
                assert!(
                    rest.starts_with(' '),
                    "glyph {glyph} must be followed by a space: {out:?}"
                );
                assert!(
                    !rest.starts_with("  "),
                    "glyph {glyph} must be followed by exactly one space: {out:?}"
                );
            }
        }
        assert!(!out.contains("  "), "no double spaces anywhere: {out:?}");
    }

    /// The retired glyphs and labels must not survive anywhere in a full render.
    #[test]
    fn retired_glyphs_are_gone() {
        let mut c = cfg();
        c.lines = vec!["{model} {context_bar} {cost} {usage} {codex} {openai_cost}".to_string()];
        let s = snap();
        let cross = CrossProvider {
            codex_five_hour: Some(12.0),
            codex_weekly: Some(7.0),
            openai_cost_micro_usd: Some(0),
            ..Default::default()
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: Some(&cross),
            config: &c,
            now: now(),
            color: false,
        });
        for retired in ['⌛', '📅', '◇'] {
            assert!(!out.contains(retired), "retired glyph {retired}: {out}");
        }
        assert!(
            !out.contains("OpenAI"),
            "the bare 'OpenAI' literal is replaced by the 🌀 glyph: {out}"
        );
    }

    /// The stale marker carries the VS16 variation selector so it advances two
    /// cells like every other glyph. A bare U+26A0 advances one and ragged the
    /// tail of the line.
    #[test]
    fn stale_marker_is_emoji_presentation() {
        let mut c = cfg();
        c.lines = vec!["{codex}".to_string()];
        let s = snap();
        let cross = CrossProvider {
            codex_five_hour: Some(12.0),
            codex_stale: true,
            ..Default::default()
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: Some(&cross),
            config: &c,
            now: now(),
            color: false,
        });
        assert!(
            out.ends_with("\u{26A0}\u{FE0F}"),
            "stale marker must be U+26A0 U+FE0F: {out:?}"
        );
    }
```

- [ ] **Step 2: Run the new tests to verify they fail**

```bash
cargo nextest run -p statusline_render
```

Expected: FAIL. `glyph_table`, `codex_weekly_is_labeled_7d_not_wk`, `every_glyph_is_followed_by_exactly_one_space`, `retired_glyphs_are_gone`, and `stale_marker_is_emoji_presentation` all fail on the current glyphs (`⌛5h`, `◇wk`, no `🧠`, bare `OpenAI`, bare `⚠`).

- [ ] **Step 3: Apply the glyph changes**

In `crates/statusline_render/src/render.rs`:

`context_bar` arm (currently around line 106) - prefix the bar with the brain glyph:

```rust
            let text = format!("🧠 {} {shown}%", bar(pct, c.width));
```

`codex` arm (currently around line 132) - relabel both windows and give the stale marker its variation selector:

```rust
        "codex" => {
            let cross = input.cross?;
            let mut windows = Vec::new();
            if let Some(pct) = cross.codex_five_hour {
                windows.push(render_codex_window("🌀 5h", pct, input));
            }
            if let Some(pct) = cross.codex_weekly {
                // "7d", not "wk": every other surface calls this window 7d, and
                // the provider glyph is what separates it from Claude's 7d.
                windows.push(render_codex_window("🌀 7d", pct, input));
            }
            if windows.is_empty() {
                return None;
            }
            let mark = if cross.codex_stale { " ⚠️" } else { "" };
            Some(format!("{}{mark}", windows.join(" ")))
        }
```

`openai_cost` arm (currently around line 147) - the provider glyph replaces the bare `OpenAI` literal:

```rust
        "openai_cost" => {
            let cross = input.cross?;
            let micro = cross.openai_cost_micro_usd?;
            let mark = if cross.openai_stale { " ⚠️" } else { "" };
            Some(paint(
                &format!("🌀 {}{mark}", fmt_money(micro)),
                resolve(&segs.openai_cost.style, theme, "openai_cost", Tone::Base),
                "",
                "",
                Tone::Base,
                input.color,
            ))
        }
```

`render_usage` (currently around line 165) - Claude's provider glyph replaces the duration glyphs:

```rust
    if let Some(w) = rl.five_hour() {
        windows.push(render_window("✳️ 5h", w, Duration::hours(5), c, input));
    }
    if let Some(w) = rl.seven_day() {
        windows.push(render_window("✳️ 7d", w, Duration::days(7), c, input));
    }
```

- [ ] **Step 4: Update the existing tests that pin the old glyphs**

In `crates/statusline_render/src/render.rs` tests:

- `codex_segment_colored_by_severity_band`: change `assert!(out.contains("◇5h 80%"), "codex label: {out}");` to `assert!(out.contains("🌀 5h 80%"), "codex label: {out}");`
- `codex_severity_classifies_rounded_value_at_cutoff`: change `assert!(out.contains("◇5h 90%"), "codex rounds to 90: {out}");` to `assert!(out.contains("🌀 5h 90%"), "codex rounds to 90: {out}");`
- `codex_renders_both_windows`: change the two assertions to

```rust
        assert!(out.contains("🌀 5h 2%"), "5h window: {out}");
        assert!(out.contains("🌀 7d 3%"), "weekly window labeled 7d: {out}");
```

In `crates/balanze_cli/src/statusline.rs`, the test `cross_renders_codex_and_openai_segments` (currently around line 457) pins both old glyphs. Change its two assertions to:

```rust
        assert!(out.contains("🌀 5h 6%"), "{out}");
        assert!(out.contains("🌀 $4.20"), "{out}");
```

- [ ] **Step 5: Run the full gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Expected: all PASS. Note that `renders_default_layout_plain` asserts `out.contains("5h 82%")`, which still holds because `✳️ 5h 82%` contains that substring.

- [ ] **Step 6: Commit**

```bash
git add crates/statusline_render/src/render.rs crates/balanze_cli/src/statusline.rs
git commit -m "feat(statusline): one glyph rule, provider-named windows

Every segment renders as <emoji><space><content>. The glyph names the
provider for rate windows (Claude, Codex) and the metric for the two
Claude-only figures (context, session cost). All glyphs are now
emoji-presentation and two cells wide, including the stale marker,
which was a bare U+26A0 advancing one cell.

Codex's weekly window is relabeled 7d to match Claude and the compact
CLI surface, which already asserts that label."
```

---

### Task 2: Config-dir override in settings

`settings::default_path()` has no escape hatch, unlike `BALANZE_DATA_DIR_OVERRIDE` and `BALANZE_CACHE_DIR_OVERRIDE`. Task 4's integration test needs one to inject a `settings.json` with a custom template.

**Files:**
- Modify: `crates/settings/src/lib.rs` (`default_path`, and its `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `BALANZE_CONFIG_DIR_OVERRIDE` - when set, `settings::default_path()` returns `<that dir>/settings.json`. Task 4's integration test sets it on the `balanze-cli` subprocess.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/settings/src/lib.rs`. The module already imports `super::*` and uses `tempfile::tempdir()` elsewhere; add the `ENV_LOCK` static at the top of the test module (it does not exist there yet):

```rust
    /// Serializes env-mutating tests in this module. `cargo nextest` runs each
    /// test in its own process, but plain `cargo test` shares one, so the lock
    /// keeps both runners honest.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn default_path_honors_config_dir_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK serializes env-mutating tests in this module; restored below.
        unsafe { std::env::set_var("BALANZE_CONFIG_DIR_OVERRIDE", dir.path()) };
        let p = default_path().expect("path");
        assert_eq!(p, dir.path().join("settings.json"));
        unsafe { std::env::remove_var("BALANZE_CONFIG_DIR_OVERRIDE") };
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo nextest run -p settings default_path_honors_config_dir_override
```

Expected: FAIL - the returned path is the real `ProjectDirs` config dir, not the temp dir.

- [ ] **Step 3: Implement the override**

In `crates/settings/src/lib.rs`, replace `default_path` (currently at line 127):

```rust
/// Conventional settings.json path for this user. Lazy: doesn't create the
/// directory.
///
/// `BALANZE_CONFIG_DIR_OVERRIDE` is intended for tests that need an isolated
/// config directory, mirroring `BALANZE_DATA_DIR_OVERRIDE` and
/// `BALANZE_CACHE_DIR_OVERRIDE`.
pub fn default_path() -> Result<PathBuf, SettingsError> {
    if let Ok(dir) = std::env::var("BALANZE_CONFIG_DIR_OVERRIDE") {
        return Ok(PathBuf::from(dir).join("settings.json"));
    }
    let pd = project_dirs().ok_or(SettingsError::NoConfigDir)?;
    Ok(pd.config_dir().join("settings.json"))
}
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo nextest run -p settings
```

Expected: PASS.

- [ ] **Step 5: Run the full gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/settings/src/lib.rs
git commit -m "feat(settings): add BALANZE_CONFIG_DIR_OVERRIDE

Mirrors the existing BALANZE_DATA_DIR_OVERRIDE and
BALANZE_CACHE_DIR_OVERRIDE escape hatches so a test can point the binary
at an isolated settings.json instead of the developer's real one."
```

---

### Task 3: Demand-gate the OpenAI fetch

`self_compose` fetches the OpenAI cost unconditionally: cache read, refresh lease, and every 300s a real HTTP call to the Admin Costs API. Once `{openai_cost}` leaves the default line (Task 4), all of that would run every turn to produce a number nothing renders. Gate it on whether any configured line actually asks for the segment.

The default template still contains `{openai_cost}` after this task, so `want_openai` is `true` in the default path and observable behavior is unchanged. This task only builds the switch; Task 4 flips it.

**Files:**
- Modify: `crates/statusline_render/src/self_compose.rs` (`self_compose` signature + its 12 test call sites)
- Modify: `crates/balanze_cli/src/statusline.rs` (`statusline_cross_provider`, `self_compose_cross`, `render_line`, tests)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `statusline_render::self_compose(sources: &S, cache_dir: &Path, fingerprint: &str, now: DateTime<Utc>, want_openai: bool) -> CrossProvider`. When `want_openai` is `false` it never touches the cache or the network and returns `openai_cost_micro_usd: None, openai_stale: false`.
  - `balanze_cli::statusline::want_openai(config: &settings::StatuslineConfig) -> bool` - true when any configured line contains the literal `{openai_cost}`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/statusline_render/src/self_compose.rs`. That module already has a `t0()` helper and this fake, which every existing test constructs and binds to `f` - reuse it, do not add a second one:

```rust
    struct Fake {
        openai: Result<Option<i64>, String>,
        codex: Option<f32>,
        calls: Cell<u32>,
    }
```

Note `Fake::codex_windows` returns `(self.codex, None)`, so the fake only ever produces a 5h Codex window. Assert on `codex_five_hour` only.

```rust
    /// want_openai=false must not touch the cache or the network at all: no
    /// value, no staleness, and no fetch recorded on the fake. The Codex half
    /// is unaffected - it is local and cheap, and the gate is only about OpenAI.
    #[tokio::test]
    async fn want_openai_false_skips_the_fetch_entirely() {
        let dir = tempdir().unwrap();
        let f = Fake {
            openai: Ok(Some(4_200_000)),
            codex: Some(12.0),
            calls: Cell::new(0),
        };
        let cp = self_compose(&f, dir.path(), "fp", t0(), false).await;
        assert_eq!(cp.openai_cost_micro_usd, None, "no value when not wanted");
        assert!(!cp.openai_stale, "not stale, just absent");
        assert_eq!(f.calls.get(), 0, "no upstream fetch when not wanted");
        assert!(
            cache::read(dir.path(), "fp").is_none(),
            "the cache is not even touched when not wanted"
        );
        assert_eq!(cp.codex_five_hour, Some(12.0), "Codex still composed");
    }
```

Add to the `#[cfg(test)] mod tests` block in `crates/balanze_cli/src/statusline.rs`:

```rust
    #[test]
    fn want_openai_follows_the_configured_lines() {
        let mut asks = settings::StatuslineConfig::default();
        asks.lines = vec!["{usage} {openai_cost}".to_string()];
        assert!(super::want_openai(&asks), "template asks for the segment");

        let mut silent = settings::StatuslineConfig::default();
        silent.lines = vec!["{usage} {codex}".to_string()];
        assert!(
            !super::want_openai(&silent),
            "template does not ask for the segment"
        );
    }
```

This asserts the predicate against explicit templates only, so it is green the moment `want_openai` exists. Task 4 adds the separate assertion that the *shipped default* template does not ask for the segment - that one belongs with the change that makes it true.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo nextest run -p statusline_render want_openai
cargo nextest run -p balanze_cli want_openai
```

Expected:
- `want_openai_false_skips_the_fetch_entirely`: FAILS to compile - `self_compose` takes 4 arguments, not 5.
- `want_openai_follows_the_configured_lines`: FAILS to compile - `want_openai` is not defined.

- [ ] **Step 3: Add the parameter to `self_compose`**

In `crates/statusline_render/src/self_compose.rs`, replace `self_compose` (currently at line 40):

```rust
/// Compose the cross-provider cells without the watcher.
///
/// `want_openai` is false when no configured statusline line contains the
/// `{openai_cost}` placeholder. In that case the OpenAI cost is not fetched at
/// all - not from the cache, not from the network. The politest call to a
/// provider is the one you do not make (AGENTS.md §3.1).
pub async fn self_compose<S: CrossSources>(
    sources: &S,
    cache_dir: &Path,
    fingerprint: &str,
    now: DateTime<Utc>,
    want_openai: bool,
) -> CrossProvider {
    // Codex: local, cheap, never cached -> current whenever present.
    let (codex_five_hour, codex_weekly) = sources.codex_windows();

    let (openai_cost_micro_usd, openai_stale) = if want_openai {
        openai_value(sources, cache_dir, fingerprint, now).await
    } else {
        (None, false)
    };

    CrossProvider {
        codex_five_hour,
        codex_weekly,
        openai_cost_micro_usd,
        codex_stale: false,
        openai_stale,
    }
}
```

- [ ] **Step 4: Update the 12 existing `self_compose` call sites in that module's tests**

Every existing test in `crates/statusline_render/src/self_compose.rs` exercises the OpenAI path, so each passes `true`. They are at lines 206, 222, 223, 237, 238, 251, 267, 281, 348, 351, 377, and 399. Each currently ends `..., t0()).await` or `..., t0() + Duration::seconds(N)).await`; add `, true` before the closing paren. For example line 206:

```rust
        let cp = self_compose(&f, dir.path(), "fp", t0(), true).await;
```

and line 223:

```rust
        let cp = self_compose(&f, dir.path(), "fp", t0() + Duration::seconds(120), true).await;
```

Verify none were missed:

```bash
cargo check -p statusline_render --all-targets
```

Expected: compiles clean.

- [ ] **Step 5: Thread the gate through the CLI**

In `crates/balanze_cli/src/statusline.rs`:

Add the predicate next to `statusline_cross_provider`:

```rust
/// True when any configured line asks for the `{openai_cost}` segment. When
/// false, the self-compose path skips the OpenAI cost entirely: no cache read,
/// no refresh lease, no HTTP. The segment is off in the default template, so
/// this is the common case.
fn want_openai(config: &settings::StatuslineConfig) -> bool {
    config.lines.iter().any(|l| l.contains("{openai_cost}"))
}
```

Replace `statusline_cross_provider` (currently at line 87) so it takes the config and honors the gate. Note the freshness short-circuit: an `openai_stale` snapshot is no longer a reason to self-compose when nobody wants the value.

```rust
fn statusline_cross_provider(
    config: &settings::StatuslineConfig,
) -> Option<statusline_render::CrossProvider> {
    let now = chrono::Utc::now();
    let want_openai = want_openai(config);

    // Read the host snapshot once: it feeds both the fresh-path short-circuit and
    // the seed that lets the self-compose OpenAI gate honor the watcher's fetch.
    let payload = read_snapshot_payload();
    let snapshot_cross = payload.as_ref().map(|p| cross_from_payload(p, now));

    // 1. Fresh snapshot wins (zero network). A stale OpenAI cell is only a
    //    reason to self-compose when the OpenAI segment is actually rendered.
    if let Some(cross) = &snapshot_cross {
        if (!want_openai || !cross.openai_stale) && !cross.codex_stale {
            return snapshot_cross;
        }
    }

    // 2. Self-compose; then merge composed cells over the (stale) snapshot
    //    cells per cell so a last-known value stays visible (never-blank).
    pick_cross(self_compose_cross(now, want_openai), snapshot_cross)
}
```

Replace `self_compose_cross` (currently at line 147):

```rust
fn self_compose_cross(
    now: chrono::DateTime<chrono::Utc>,
    want_openai: bool,
) -> Option<statusline_render::CrossProvider> {
    let cache_dir = statusline_render::cache::cache_dir_path()?;
    let sources = crate::sources::LiveCrossSources::resolve_once();
    let fingerprint = sources.openai_fingerprint();
    // One-shot CLI: a fresh per-turn runtime is acceptable; the OpenAI fetch
    // inside self_compose is cache-gated (300s) and skipped entirely when the
    // segment is not configured, so the network is not hit every turn even
    // though the runtime is built every turn.
    let rt = tokio::runtime::Runtime::new().ok()?;
    Some(rt.block_on(statusline_render::self_compose(
        &sources,
        &cache_dir,
        &fingerprint,
        now,
        want_openai,
    )))
}
```

Update `render_line` (currently at line 218) to pass the config:

```rust
fn render_line(snap: &claude_statusline::StatuslineSnapshot) -> String {
    let settings = settings::load().unwrap_or_default();
    let color = std::env::var_os("NO_COLOR").is_none();
    let cross = statusline_cross_provider(&settings.statusline);
    render_with(snap, &settings.statusline, color, cross.as_ref())
}
```

The existing test `cross_provider_none_when_snapshot_absent_and_no_self_compose_data` (currently around line 522) calls `super::statusline_cross_provider()` with no argument. Update its call:

```rust
        assert!(
            super::statusline_cross_provider(&settings::StatuslineConfig::default()).is_none()
        );
```

- [ ] **Step 6: Run the gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Expected: all PASS. The default template still contains `{openai_cost}`, so `want_openai` is `true` on the default path and observable behavior is unchanged. This task builds the switch; Task 4 flips it.

- [ ] **Step 7: Commit**

```bash
git add crates/statusline_render/src/self_compose.rs crates/balanze_cli/src/statusline.rs
git commit -m "feat(statusline): demand-gate the OpenAI cost fetch

self_compose fetched the OpenAI cost on every turn regardless of whether
any configured line rendered it: a cache read, a refresh lease, and an
HTTP call every 300s. It now takes want_openai and skips the whole path
when no line contains {openai_cost}.

The freshness short-circuit stops treating a stale OpenAI cell as a
reason to self-compose when the value is not wanted."
```

---

### Task 4: Drop `{openai_cost}` from the default line

This flips the gate built in Task 3 and rewires the self-compose integration test, which currently asserts on a segment the default template no longer renders. Both must land together to keep the tree green.

**Files:**
- Modify: `crates/settings/src/statusline.rs` (`default_lines`, tests)
- Modify: `crates/balanze_cli/src/statusline.rs` (one new test)
- Modify: `crates/balanze_cli/tests/integration_statusline_self_compose.rs`

**Interfaces:**
- Consumes: `want_openai` and the gated `self_compose` from Task 3; `BALANZE_CONFIG_DIR_OVERRIDE` from Task 2; the `🌀 $4.20` render from Task 1.
- Produces: `default_lines()` returning `["{model} {agent}", "{context_bar} {cost} {usage} {codex}"]`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/settings/src/statusline.rs`:

```rust
    /// OpenAI API spend is an uncapped dollar figure with no rolling window, so
    /// it does not belong on a default line that is otherwise percent-of-window.
    /// The segment stays implemented and configurable; it is just off by default.
    #[test]
    fn default_lines_omit_the_openai_cost_segment() {
        let lines = default_lines();
        assert!(
            !lines.iter().any(|l| l.contains("{openai_cost}")),
            "openai_cost must be off by default: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("{codex}")),
            "the Codex windows stay on the default line: {lines:?}"
        );
    }
```

And add the companion assertion in `crates/balanze_cli/src/statusline.rs`, next to `want_openai_follows_the_configured_lines` from Task 3. This is the assertion that ties the predicate to the shipped default, and it only becomes true with the change in Step 3:

```rust
    #[test]
    fn default_template_does_not_want_openai() {
        assert!(
            !super::want_openai(&settings::StatuslineConfig::default()),
            "the shipped default template must not request the OpenAI segment"
        );
    }
```

- [ ] **Step 2: Run it to verify it fails**

```bash
cargo nextest run -p settings default_lines_omit_the_openai_cost_segment
cargo nextest run -p balanze_cli default_template_does_not_want_openai
```

Expected: both FAIL - the default second line still contains `{openai_cost}`.

- [ ] **Step 3: Change the default template**

In `crates/settings/src/statusline.rs`, replace `default_lines` (currently at line 55):

```rust
fn default_lines() -> Vec<String> {
    vec![
        "{model} {agent}".to_string(),
        // `openai_cost` is deliberately absent: it is an uncapped dollar figure
        // with no rolling window, so it does not read against a line that is
        // otherwise percent-of-window. It stays implemented and configurable.
        "{context_bar} {cost} {usage} {codex}".to_string(),
    ]
}
```

- [ ] **Step 4: Rewire the self-compose integration test**

Replace `crates/balanze_cli/tests/integration_statusline_self_compose.rs` in full. Two cases: the configured template still fetches and renders (and still collapses two renders into one GET), and the default template fetches nothing at all.

```rust
//! Self-compose end-to-end: no snapshot present, OpenAI composed directly and
//! gated to one fetch per 300s. Drives the real `balanze-cli statusline` binary
//! against a wiremock Admin Costs API.

use assert_cmd::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Minimal valid `/v1/organization/costs` body. Shape matches what
/// `openai_client` parses (verified against `crates/openai_client/tests/`);
/// value 4.20 USD -> total_micro_usd 4_200_000 -> rendered "🌀 $4.20".
fn costs_body() -> serde_json::Value {
    serde_json::json!({
        "object": "page",
        "data": [{
            "object": "bucket",
            "start_time": 0,
            "end_time": 1,
            "results": [{
                "object": "organization.costs.result",
                "amount": { "value": 4.20, "currency": "usd" },
                "line_item": "gpt-5"
            }]
        }],
        "has_more": false
    })
}

/// A settings.json whose statusline template asks for `{openai_cost}`. Without
/// this the segment is off by default and the fetch is demand-gated away.
fn settings_json_requesting_openai() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "statusline": {
            "lines": ["{context_bar} {cost} {usage} {codex} {openai_cost}"]
        }
    })
}

/// Run `balanze-cli statusline` once against the given dirs. `config_dir` is
/// `None` to exercise the shipped default template.
fn run_statusline(
    data: &std::path::Path,
    cache: &std::path::Path,
    codex: &std::path::Path,
    base: &str,
    config: Option<&std::path::Path>,
) -> std::process::Output {
    let mut cmd = Command::cargo_bin("balanze-cli").unwrap();
    cmd.arg("statusline")
        .env("BALANZE_DATA_DIR_OVERRIDE", data)
        .env("BALANZE_CACHE_DIR_OVERRIDE", cache)
        .env("BALANZE_OPENAI_API_BASE", base)
        .env("BALANZE_OPENAI_KEY", "sk-test")
        .env("CODEX_CONFIG_DIR", codex)
        .env("NO_COLOR", "1")
        .write_stdin(r#"{"version":"2.1.144","model":{"display_name":"Sonnet"}}"#);
    match config {
        Some(dir) => cmd.env("BALANZE_CONFIG_DIR_OVERRIDE", dir),
        // An empty temp dir has no settings.json -> the shipped defaults load.
        None => cmd.env("BALANZE_CONFIG_DIR_OVERRIDE", data),
    };
    cmd.output().unwrap()
}

#[tokio::test]
async fn self_compose_renders_openai_and_gates_to_one_fetch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(costs_body()))
        .expect(1) // the 300s cache must collapse two renders into one fetch
        .mount(&server)
        .await;

    let data_dir = tempfile::tempdir().unwrap(); // empty -> no snapshot.json -> self-compose
    let cache_dir = tempfile::tempdir().unwrap();
    let codex_dir = tempfile::tempdir().unwrap(); // empty -> Codex absent, focus on OpenAI
    let config_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        config_dir.path().join("settings.json"),
        serde_json::to_vec(&settings_json_requesting_openai()).unwrap(),
    )
    .unwrap();
    let base = server.uri();

    // Two renders within the TTL: the cache must yield exactly one upstream GET.
    for _ in 0..2 {
        let (data, cache, codex, config, base) = (
            data_dir.path().to_path_buf(),
            cache_dir.path().to_path_buf(),
            codex_dir.path().to_path_buf(),
            config_dir.path().to_path_buf(),
            base.clone(),
        );
        let out = tokio::task::spawn_blocking(move || {
            run_statusline(&data, &cache, &codex, &base, Some(&config))
        })
        .await
        .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "balanze-cli statusline exited {:?};\nstderr: {stderr}\nstdout: {stdout}",
            out.status,
        );
        assert!(
            stdout.contains("🌀 $4.20"),
            "self-composed OpenAI segment missing;\nstdout: {stdout}\nstderr: {stderr}"
        );
    }
    // `server` drops here; `.expect(1)` is verified on drop ->
    // two renders, one fetch proves the 300s gate.
}

/// The demand gate: with the shipped default template the OpenAI segment is not
/// rendered, so the billing API must not be called at all - not once, not
/// cached. This is the regression test for the gate.
#[tokio::test]
async fn default_template_never_fetches_openai() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(costs_body()))
        .expect(0) // no line asks for the segment -> no upstream call
        .mount(&server)
        .await;

    let data_dir = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let codex_dir = tempfile::tempdir().unwrap();
    let base = server.uri();

    let (data, cache, codex, b) = (
        data_dir.path().to_path_buf(),
        cache_dir.path().to_path_buf(),
        codex_dir.path().to_path_buf(),
        base.clone(),
    );
    let out = tokio::task::spawn_blocking(move || run_statusline(&data, &cache, &codex, &b, None))
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "balanze-cli statusline exited {:?};\nstderr: {stderr}\nstdout: {stdout}",
        out.status,
    );
    assert!(
        !stdout.contains("🌀 $"),
        "the OpenAI cost segment must not render under the default template;\nstdout: {stdout}"
    );
    // `server` drops here; `.expect(0)` is verified on drop.
}
```

- [ ] **Step 5: Run the gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Expected: all PASS.

- [ ] **Step 6: Verify against the real binary**

The renderer is pure and unit-tested, but the point of this change is how it looks. Render a line with a synthetic payload:

```bash
echo '{"version":"2.1.144","model":{"display_name":"Opus"}}' | cargo run -q -p balanze_cli -- statusline
```

Expected: a line beginning `🤖 Opus` on one row and `🧠 [` on the next, with `✳️ 5h`/`✳️ 7d` for Claude windows if the OAuth snapshot is present, `🌀 5h`/`🌀 7d` if Codex data is present, and no `OpenAI $` anywhere. Confirm the emoji are followed by exactly one space and nothing looks ragged in your terminal.

- [ ] **Step 7: Commit**

```bash
git add crates/settings/src/statusline.rs crates/balanze_cli/src/statusline.rs crates/balanze_cli/tests/integration_statusline_self_compose.rs
git commit -m "feat(statusline): drop OpenAI spend from the default line

OpenAI API spend is an uncapped dollar figure with no rolling window, so
it never read against a line that is otherwise percent-of-window. The
segment stays implemented and configurable; it is just off by default,
and its fetch is now demand-gated away with it.

The self-compose integration test injects a template that asks for the
segment, and a second case asserts the default template makes zero calls
to the billing API."
```

---

### Task 5: Documentation

Three docs advertise OpenAI spend as a default statusline segment, and two of them call the Codex weekly window "weekly" rather than 7d.

**Files:**
- Modify: `README.md:120`
- Modify: `docs/GUIDE.md:80`
- Modify: `docs/TROUBLESHOOTING.md:45`
- Modify: `crates/settings/src/statusline.rs` (module doc, `lines` doc comment)
- Modify: `CHANGELOG.md`

**Interfaces:**
- Consumes: the final behavior from Tasks 1-4.
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Update `README.md:120`**

The sentence currently reads "...plus cross-provider signal (both Codex rate-limit windows, 5h and weekly, and real OpenAI spend), with no rate limit." Replace the parenthetical so it names the 7d label and marks OpenAI spend as opt-in:

```markdown
**Claude Code statusLine.** `balanze-cli statusline` is a zero-auth status line for your Claude Code prompt - live 5h/7d Claude subscription quota and session cost, plus cross-provider signal (both Codex rate-limit windows, 5h and 7d), with no rate limit. Real OpenAI API spend is available as an opt-in `{openai_cost}` segment; it is off by default because it is an uncapped dollar figure with no rolling window. Concurrent prompt processes share one atomically published OpenAI cache and refresh lease, so stale data remains available without duplicating upstream requests. `balanze-cli setup` offers to wire the exact canonical `balanze-cli statusline` command; wrappers and composed commands remain foreign unless the user explicitly replaces them. A replaced command is backed up so `balanze-cli statusline restore` can put it back.
```

- [ ] **Step 2: Update `docs/GUIDE.md:80`**

Currently: "...and - uniquely - cross-provider signal (both Codex rate-limit windows, 5-hour and weekly, and real OpenAI spend) in one line." Replace with:

```markdown
`balanze-cli statusline` is a zero-auth status line for your Claude Code prompt: live 5h/7d subscription quota, session cost, and - uniquely - cross-provider signal (both Codex rate-limit windows, 5h and 7d) in one line. Real OpenAI API spend is available as an opt-in `{openai_cost}` segment, off by default.
```

- [ ] **Step 3: Update `docs/TROUBLESHOOTING.md:45`**

The heading and body both imply the OpenAI segment is on by default. Replace lines 45-47 in full:

```markdown
## "Cross-provider segments (Codex %, and OpenAI $ if you enabled it) appear in the statusline even when the desktop app / watcher is not running"

Expected behavior - not a bug. `balanze-cli statusline` self-composes these segments when no fresh `snapshot.json` exists: Codex is read directly from local files, and OpenAI cost - **if** a configured line contains `{openai_cost}` - is fetched from the Admin Costs API and cached in `<cache>/statusline/openai-cost.json` for up to 5 minutes. At most one upstream OpenAI request fires per 300 seconds across all concurrent turns (the §3.1 politeness gate). If a fetch fails, the last known value is served with a `⚠️` marker rather than blanking the segment; the endpoint is not retried for 60 seconds. With the default template the OpenAI segment is off, and the statusline makes no calls to the billing API at all. Starting the desktop app or `balanze-cli watch` produces a fresh `snapshot.json` which takes precedence and the self-compose path is bypassed entirely.
```

The old body opened with "Since PR3, ..." - that release nomenclature is dropped, per AGENTS.md §8's rule against project-management framing in durable content.

- [ ] **Step 4: Update the settings module docs**

In `crates/settings/src/statusline.rs`, the module doc at line 15 says an explicit style overrides the theme palette "for the `model`, `context_bar`, `cost`, and `openai_cost` segments" - still true, leave it. The `lines` field doc at line 37 lists the placeholders; add a note that `openai_cost` is off by default:

```rust
    /// Line templates: each is a space-separated layout of `{segment}`
    /// placeholders (model, agent, context_bar, cost, usage, codex,
    /// openai_cost). Empty segments are dropped; literal text is kept.
    /// `openai_cost` is available but absent from the default lines - see
    /// `default_lines`.
    #[serde(default = "default_lines")]
    pub lines: Vec<String>,
```

- [ ] **Step 5: Add the changelog entry**

Add to the Unreleased section of `CHANGELOG.md` (create the section if it does not exist, matching the file's existing heading style):

```markdown
- **Statusline glyph grammar** - every segment now renders as `<emoji> <content>` with the emoji naming the provider: `✳️` for Claude's windows, `🌀` for Codex, `🧠` for context, `💰` for the session cost estimate. All glyphs are emoji-presentation and two cells wide, so the line no longer looks ragged. Codex's weekly window is relabeled `7d`, matching Claude and the CLI.
- **OpenAI API spend is off the default statusline** - it is an uncapped dollar figure with no rolling window, so it never read against a line that is otherwise percent-of-window. The `{openai_cost}` segment stays available; add it to `statusline.lines` in `settings.json` to bring it back. With it off, the statusline makes no calls to the OpenAI billing API at all. **If you already have a `settings.json`, your `statusline.lines` is pinned to the old template** - delete that key (or edit it) to pick up the new default.
```

- [ ] **Step 6: Verify no em-dashes or Unicode ellipses crept in**

```bash
git diff --cached -U0 | grep -nP '^\+.*[\x{2014}\x{2026}]' && echo "VIOLATION: em-dash or ellipsis found" || echo "clean"
```

Expected: `clean`. (AGENTS.md §3.5 forbids both.) Run this after staging in the next step if the diff is empty here.

- [ ] **Step 7: Commit**

```bash
git add README.md docs/GUIDE.md docs/TROUBLESHOOTING.md crates/settings/src/statusline.rs CHANGELOG.md
git commit -m "docs(statusline): describe the glyph grammar and the opt-in OpenAI segment

Three docs advertised OpenAI spend as a default statusline segment and
called the Codex weekly window 'weekly' rather than 7d. The changelog
notes that a persisted statusline.lines pins the old template."
```

---

## Final Verification

- [ ] **Full gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
bun run check
```

All must pass. Note the CI split (AGENTS.md §6): the per-PR `linux` job does not lint `src-tauri`. Nothing in this plan touches `src-tauri`, so that gap is not a risk here.

- [ ] **Manual smoke**

```bash
echo '{"version":"2.1.144","model":{"display_name":"Opus"}}' | cargo run -q -p balanze_cli -- statusline
```

Confirm in your own terminal: uniform spacing, no ragged tail, `🌀 7d` for the Codex weekly window, no `OpenAI $`.

- [ ] **Open the PR**

Squash-merge lands the PR title on `main`, so the title must be a valid Conventional Commit (CI validates this via `.github/workflows/pr-title.yml`). Suggested title:

```
feat(statusline): one glyph rule, 7d Codex label, OpenAI spend off by default
```

## Out of Scope

- An OpenAI budget setting that would let the segment show a percent and pick up the shared 50/75/90 severity band. That is a `Settings` schema change and needs its own review (AGENTS.md §8).
- Reinstating `openai_cost` with a month period and a countdown to the 1st. Considered and declined during design.
- `docs/reviews/surface-consistency.html` contains a rendered sample line with the old glyphs. It is a dated review artifact, not live documentation - leave it alone.
