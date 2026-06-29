# Statusline PR1: Segment Engine + Config + Colored Claude Line - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `statusline_render` crate (segment model, line layout, threshold coloring), add a `statusline` config section to `settings.json`, extend the `claude_statusline` parser with the fields the line needs, and rewire `balanze-cli statusline` to render a configurable, colored, Claude-only line.

**Architecture:** A new pure crate `statusline_render` owns the segment renderers, the `{key}` line-template substitution, and a small style-string -> 24-bit ANSI parser. Config data (theme, line templates, per-segment styles + integer thresholds) lives in the `settings` crate (data), consumed by `statusline_render` (behavior). The CLI command stays thin glue: parse stdin -> load settings -> render -> print, with the existing IPC snapshot write untouched. Cross-provider segments (codex, openai_cost) and the per-turn cache are out of scope here (PR2/PR3); their placeholders render empty for now.

**Tech Stack:** Rust 2024, `serde`, `chrono`, `tracing`, `cargo nextest`. Reuses `window::pace` for the pace annotation. No networking in this PR.

**Scope note:** This is PR1 of the 5-PR v0.4.2 statusline release defined in `docs/superpowers/specs/2026-06-30-statusline-design.md`. PR2-PR5 (cross-provider via watcher snapshot, self-compose + cache, replace-any-statusline flow, Codex preset + docs/release) get their own plans authored at each PR boundary once this lands. `agent` parsing is deferred (no `agent` field appears in a normal statusLine payload; it needs a subagent-active capture) - its placeholder renders empty until a later PR.

**§2.1 currency rule:** the spec's illustrative config showed a float cost threshold (`"warn": 2.0`). This plan uses **i64 micro-USD** for the cost threshold and **u32 percent** for the rest, per AGENTS.md §2.1 ("never f64 for threshold comparisons"). This also keeps `Settings` deriving `Eq`.

---

### Task 1: Scaffold `statusline_render` crate + style-string -> ANSI parser

**Goal:** Create the new crate and its pure `style` module that turns a cship-like style spec (`"bold fg:#7aa2f7 bg:#1a1b26 italic underline"`) into a 24-bit ANSI wrapper.

**Files:**
- Create: `crates/statusline_render/Cargo.toml`
- Create: `crates/statusline_render/src/lib.rs`
- Create: `crates/statusline_render/src/style.rs`
- (No workspace `Cargo.toml` edit needed: `members = ["src-tauri", "crates/*"]` already globs the new crate.)

**Acceptance Criteria:**
- [ ] `cargo build -p statusline_render` succeeds.
- [ ] `style::apply_style("", "x")` returns `"x"` (no escapes for a blank spec).
- [ ] `style::apply_style("bold fg:#7aa2f7", "x")` returns `"\x1b[1;38;2;122;162;247mx\x1b[0m"`.
- [ ] Invalid hex (`fg:#zzzz`) and unknown tokens are ignored, not errors.

**Verify:** `cargo nextest run -p statusline_render` -> all green; `cargo clippy -p statusline_render --all-targets -- -D warnings` -> clean.

**Steps:**

- [ ] **Step 1: Create `crates/statusline_render/Cargo.toml`**

```toml
[package]
name = "statusline_render"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
publish.workspace = true
description = "Renders the Balanze statusline: segment model, line layout, and threshold coloring. Later PRs add the per-turn cache and cross-provider composition."

[dependencies]
claude_statusline = { path = "../claude_statusline" }
settings = { path = "../settings" }
window = { path = "../window" }
chrono = { workspace = true }
tracing = { workspace = true }
```

- [ ] **Step 2: Create `crates/statusline_render/src/lib.rs`**

```rust
//! Renders the Balanze statusline. Pure: a parsed statusLine snapshot + the
//! `settings::StatuslineConfig` + a clock instant -> the lines Claude Code
//! prints. Threshold coloring, the line-template layout, and the style-string
//! parser live here; the config DATA lives in the `settings` crate.
//!
//! Cross-provider segments (codex, openai_cost) and the per-turn cache arrive
//! in later PRs; their placeholders render empty until then.

pub mod style;
// `render` module is added in Task 4.
```

- [ ] **Step 3: Write failing tests in `crates/statusline_render/src/style.rs`**

```rust
//! Minimal style-string parser: turns a cship-like style spec
//! ("bold fg:#7aa2f7 bg:#1a1b26 italic underline") into a 24-bit ANSI escape
//! prefix + reset, applied around a segment's text. Unknown tokens and invalid
//! hex are ignored (forward-compatible), never an error.

/// Wrap `text` in the ANSI escapes described by `spec`. A blank spec (or one
/// with no recognized tokens) returns `text` unchanged - no escapes.
pub fn apply_style(spec: &str, text: &str) -> String {
    let codes = ansi_codes(spec);
    if codes.is_empty() {
        return text.to_string();
    }
    format!("\x1b[{}m{}\x1b[0m", codes.join(";"), text)
}

/// Parse a style spec into ANSI SGR parameter fragments (no escape framing).
/// Exposed for unit testing.
pub fn ansi_codes(spec: &str) -> Vec<String> {
    let mut codes = Vec::new();
    for tok in spec.split_whitespace() {
        match tok {
            "bold" => codes.push("1".to_string()),
            "italic" => codes.push("3".to_string()),
            "underline" => codes.push("4".to_string()),
            _ => {
                if let Some(hex) = tok.strip_prefix("fg:#") {
                    if let Some((r, g, b)) = parse_hex(hex) {
                        codes.push(format!("38;2;{r};{g};{b}"));
                    }
                } else if let Some(hex) = tok.strip_prefix("bg:#") {
                    if let Some((r, g, b)) = parse_hex(hex) {
                        codes.push(format!("48;2;{r};{g};{b}"));
                    }
                }
                // Unrecognized token: ignore (forward-compat).
            }
        }
    }
    codes
}

fn parse_hex(hex: &str) -> Option<(u8, u8, u8)> {
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_spec_returns_text_unchanged() {
        assert_eq!(apply_style("", "x"), "x");
        assert_eq!(apply_style("   ", "x"), "x");
    }

    #[test]
    fn bold_and_fg_combine_in_order() {
        assert_eq!(ansi_codes("bold fg:#7aa2f7"), vec!["1", "38;2;122;162;247"]);
        assert_eq!(
            apply_style("bold fg:#7aa2f7", "x"),
            "\x1b[1;38;2;122;162;247mx\x1b[0m"
        );
    }

    #[test]
    fn bg_and_attrs_parse() {
        assert_eq!(
            ansi_codes("italic underline bg:#1a1b26"),
            vec!["3", "4", "48;2;26;27;38"]
        );
    }

    #[test]
    fn invalid_hex_and_unknown_tokens_ignored() {
        assert!(ansi_codes("fg:#zzzzzz").is_empty());
        assert!(ansi_codes("fg:#abc").is_empty()); // wrong length
        assert!(ansi_codes("sparkle wobble").is_empty());
        assert_eq!(ansi_codes("bogus bold"), vec!["1"]);
    }
}
```

- [ ] **Step 4: Run tests to verify they fail then pass**

Run: `cargo nextest run -p statusline_render`
Expected: compiles, all `style` tests PASS (the module is self-contained, so this is green once Steps 1-3 land).

- [ ] **Step 5: Lint + commit**

Run: `cargo clippy -p statusline_render --all-targets -- -D warnings` (expect clean), then `cargo fmt -p statusline_render`.

```bash
git add crates/statusline_render/
git commit -m "feat(statusline): scaffold statusline_render crate + style->ANSI parser"
```

---

### Task 2: `StatuslineConfig` section in `settings`

**Goal:** Add a curated, additive `statusline` config (theme, line templates, per-segment styles + integer thresholds) to `settings::Settings`, defaulting to the user-cship-matched values, with no schema version bump.

**Files:**
- Create: `crates/settings/src/statusline.rs`
- Modify: `crates/settings/src/lib.rs` (add `pub mod statusline;`, re-export, the `statusline` field on `Settings`, and the `Default` wiring + tests)

**Acceptance Criteria:**
- [ ] `StatuslineConfig::default().theme == "dark"`, `lines` non-empty, `segments.usage.show_pace == true`, `segments.cost.warn_micro_usd == 2_000_000`, `segments.context_bar.warn == 40`, `segments.usage.warn == 70`.
- [ ] An old `settings.json` (`{"version":1,"providers":{...}}`) loads with `statusline == StatuslineConfig::default()` (additive serde-default, no version bump).
- [ ] A custom `statusline` config round-trips through `save_to` / `load_from`.
- [ ] `Settings` still derives `Eq` (all thresholds are integer).

**Verify:** `cargo nextest run -p settings` -> all green; `cargo clippy -p settings --all-targets -- -D warnings` -> clean.

**Steps:**

- [ ] **Step 1: Write failing tests** (append to the `tests` module in `crates/settings/src/lib.rs`)

```rust
    #[test]
    fn statusline_defaults_are_curated() {
        let c = crate::statusline::StatuslineConfig::default();
        assert_eq!(c.theme, "dark");
        assert!(!c.lines.is_empty(), "default lines present");
        assert!(c.segments.usage.show_pace);
        assert!(c.segments.usage.show_reset);
        assert_eq!(c.segments.cost.warn_micro_usd, 2_000_000);
        assert_eq!(c.segments.cost.critical_micro_usd, 5_000_000);
        assert_eq!(c.segments.context_bar.warn, 40);
        assert_eq!(c.segments.context_bar.critical, 70);
        assert_eq!(c.segments.usage.warn, 70);
        assert_eq!(c.segments.usage.critical, 90);
    }

    #[test]
    fn statusline_absent_defaults_to_curated() {
        // Old settings.json written before the statusline section existed must
        // default it to the curated config (no version bump - additive field).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{"version":1,"providers":{"openai_enabled":false,"anthropic_enabled":true,"codex_enabled":true}}"#,
        )
        .unwrap();
        let s = load_from(&path).expect("load");
        assert_eq!(s.statusline, crate::statusline::StatuslineConfig::default());
    }

    #[test]
    fn statusline_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.statusline.theme = "light".to_string();
        s.statusline.segments.cost.warn_micro_usd = 9_000_000;
        save_to(&s, &path).expect("save");
        let loaded = load_from(&path).expect("load");
        assert_eq!(s, loaded);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p settings`
Expected: FAIL to compile (`crate::statusline` and `Settings.statusline` do not exist yet).

- [ ] **Step 3: Create `crates/settings/src/statusline.rs`**

```rust
//! Statusline display configuration: theme, line templates, and per-segment
//! styles + thresholds. Lives in `settings` because it is config DATA; the
//! rendering behavior that consumes it lives in the `statusline_render` crate.
//!
//! Thresholds use integer percent (`u32`) and i64 micro-USD (AGENTS.md §2.1:
//! never f64 for threshold comparisons), which also keeps `Settings: Eq`.
//! Default values match the maintainer's cship taste so a switch is visually
//! lossless; everything is overridable.

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatuslineConfig {
    /// "dark" | "light": palette for segments whose style is left blank.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Line templates: each is a space-separated layout of `{segment}`
    /// placeholders (model, agent, context_bar, cost, usage, codex,
    /// openai_cost). Empty segments are dropped; literal text is kept.
    #[serde(default = "default_lines")]
    pub lines: Vec<String>,
    #[serde(default)]
    pub segments: SegmentConfigs,
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_lines() -> Vec<String> {
    vec![
        "{model} {agent}".to_string(),
        "{context_bar} {cost} {usage} {codex} {openai_cost}".to_string(),
    ]
}

impl Default for StatuslineConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            lines: default_lines(),
            segments: SegmentConfigs::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentConfigs {
    #[serde(default)]
    pub model: StyleOnly,
    #[serde(default)]
    pub agent: StyleOnly,
    #[serde(default)]
    pub context_bar: ContextSegment,
    #[serde(default)]
    pub cost: MoneySegment,
    #[serde(default)]
    pub usage: UsageSegment,
    #[serde(default)]
    pub codex: PctSegment,
    #[serde(default)]
    pub openai_cost: StyleOnly,
}

impl Default for SegmentConfigs {
    fn default() -> Self {
        Self {
            model: StyleOnly { style: "bold fg:#7aa2f7".to_string() },
            agent: StyleOnly { style: "fg:#9ece6a".to_string() },
            context_bar: ContextSegment::default(),
            cost: MoneySegment::default(),
            usage: UsageSegment::default(),
            codex: PctSegment {
                warn: 70,
                critical: 90,
                style: "fg:#7dcfff".to_string(),
                warn_style: "fg:#e0af68".to_string(),
                critical_style: "bold fg:#f7768e".to_string(),
            },
            openai_cost: StyleOnly { style: "fg:#a9b1d6".to_string() },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StyleOnly {
    #[serde(default)]
    pub style: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSegment {
    #[serde(default)]
    pub warn: u32,
    #[serde(default)]
    pub critical: u32,
    #[serde(default = "default_bar_width")]
    pub width: u32,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub warn_style: String,
    #[serde(default)]
    pub critical_style: String,
}

fn default_bar_width() -> u32 {
    10
}

impl Default for ContextSegment {
    fn default() -> Self {
        Self {
            warn: 40,
            critical: 70,
            width: default_bar_width(),
            style: "fg:#7dcfff".to_string(),
            warn_style: "fg:#e0af68".to_string(),
            critical_style: "bold fg:#f7768e".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoneySegment {
    /// Warn at/above this many micro-USD (i64 per §2.1).
    #[serde(default)]
    pub warn_micro_usd: i64,
    #[serde(default)]
    pub critical_micro_usd: i64,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub warn_style: String,
    #[serde(default)]
    pub critical_style: String,
}

impl Default for MoneySegment {
    fn default() -> Self {
        Self {
            warn_micro_usd: 2_000_000,
            critical_micro_usd: 5_000_000,
            style: "fg:#a9b1d6".to_string(),
            warn_style: "fg:#e0af68".to_string(),
            critical_style: "bold fg:#f7768e".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSegment {
    #[serde(default)]
    pub warn: u32,
    #[serde(default)]
    pub critical: u32,
    #[serde(default = "default_true")]
    pub show_pace: bool,
    #[serde(default = "default_true")]
    pub show_reset: bool,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub warn_style: String,
    #[serde(default)]
    pub critical_style: String,
}

impl Default for UsageSegment {
    fn default() -> Self {
        Self {
            warn: 70,
            critical: 90,
            show_pace: true,
            show_reset: true,
            style: "fg:#a9b1d6".to_string(),
            warn_style: "fg:#e0af68".to_string(),
            critical_style: "bold fg:#f7768e".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PctSegment {
    #[serde(default)]
    pub warn: u32,
    #[serde(default)]
    pub critical: u32,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub warn_style: String,
    #[serde(default)]
    pub critical_style: String,
}

impl Default for PctSegment {
    fn default() -> Self {
        Self {
            warn: 70,
            critical: 90,
            style: "fg:#7dcfff".to_string(),
            warn_style: "fg:#e0af68".to_string(),
            critical_style: "bold fg:#f7768e".to_string(),
        }
    }
}
```

- [ ] **Step 4: Wire into `crates/settings/src/lib.rs`**

Add the module + re-export near the top (after the `use` block):

```rust
pub mod statusline;
pub use statusline::StatuslineConfig;
```

Add the field to `Settings` (after `seen_welcome`):

```rust
    /// Statusline display configuration (segments, styles, thresholds, theme).
    /// Additive serde-default: an older settings.json gets the curated default
    /// (no schema version bump). Consumed by the `statusline_render` crate.
    #[serde(default)]
    pub statusline: StatuslineConfig,
```

Add it to `impl Default for Settings`:

```rust
            statusline: StatuslineConfig::default(),
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p settings`
Expected: PASS, including the three new tests and the existing `default_settings_have_current_schema_version` (unchanged - `Settings` still `Eq`).

- [ ] **Step 6: Lint + commit**

Run: `cargo clippy -p settings --all-targets -- -D warnings`, then `cargo fmt -p settings`.

```bash
git add crates/settings/
git commit -m "feat(settings): add curated statusline display config section"
```

---

### Task 3: Extend `claude_statusline` parser with model + context fields

**Goal:** Surface `model.display_name` and `context_window.used_percentage` from the stdin payload on `StatuslineSnapshot`, with drift tolerance matching the existing parser.

**Files:**
- Modify: `crates/claude_statusline/src/types.rs` (two new fields on `StatuslineSnapshot`)
- Modify: `crates/claude_statusline/src/parse.rs` (raw structs + mapping + tests)

**Acceptance Criteria:**
- [ ] `parse(FULL).model_display_name == Some("Opus")` and `context_used_percent ~= 4.2`.
- [ ] A payload without `model` / `context_window` yields `None` for both (no error).
- [ ] The existing parser tests still pass unchanged.

**Verify:** `cargo nextest run -p claude_statusline` -> all green; `cargo clippy -p claude_statusline --all-targets -- -D warnings` -> clean.

**Steps:**

- [ ] **Step 1: Add fields to `StatuslineSnapshot` in `crates/claude_statusline/src/types.rs`**

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatuslineSnapshot {
    pub rate_limits: Option<RateLimits>,
    pub session_cost_micro_usd: Option<i64>,
    pub claude_code_version: Option<String>,
    /// Human model name from `model.display_name` (e.g. "Opus 4.7 (1M context)").
    pub model_display_name: Option<String>,
    /// Context-window utilization percent from `context_window.used_percentage`.
    pub context_used_percent: Option<f32>,
}
```

- [ ] **Step 2: Write failing test in `crates/claude_statusline/src/parse.rs`** (append to the `tests` module)

```rust
    #[test]
    fn parses_model_and_context_window() {
        // FULL already carries model.display_name "Opus" and
        // context_window.used_percentage 4.2.
        let s = parse(FULL).expect("parses");
        assert_eq!(s.model_display_name.as_deref(), Some("Opus"));
        assert!((s.context_used_percent.unwrap() - 4.2).abs() < 1e-4);
    }

    #[test]
    fn model_and_context_absent_are_none() {
        let s = parse(r#"{"version":"2.1.140"}"#).expect("parses");
        assert!(s.model_display_name.is_none());
        assert!(s.context_used_percent.is_none());
    }
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo nextest run -p claude_statusline`
Expected: FAIL to compile (the new `StatuslineSnapshot` fields aren't set in `parse`).

- [ ] **Step 4: Add raw structs + mapping in `crates/claude_statusline/src/parse.rs`**

Add to `RawRoot`:

```rust
#[derive(Debug, Deserialize)]
struct RawRoot {
    version: Option<String>,
    cost: Option<RawCost>,
    rate_limits: Option<RawRateLimits>,
    model: Option<RawModel>,
    context_window: Option<RawContextWindow>,
}
```

Add the two raw structs (next to `RawCost`):

```rust
#[derive(Debug, Deserialize)]
struct RawModel {
    // Absent or null display_name degrades to None (plain Option semantics):
    // a missing model name must never blank the whole line.
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawContextWindow {
    // Integer in real payloads (e.g. 83), fractional in others (4.2) - f32.
    used_percentage: Option<f32>,
}
```

Set the new fields in the `Ok(StatuslineSnapshot { ... })` at the end of `parse`:

```rust
    Ok(StatuslineSnapshot {
        rate_limits,
        session_cost_micro_usd,
        claude_code_version: raw.version,
        model_display_name: raw.model.and_then(|m| m.display_name),
        context_used_percent: raw.context_window.and_then(|c| c.used_percentage),
    })
```

- [ ] **Step 5: Update other `StatuslineSnapshot` constructors**

The CLI test `write_statusline_snapshot_lands_at_data_dir_override` in `crates/balanze_cli/src/statusline.rs` builds a `StatuslineSnapshot` literal; it will fail to compile until Task 6. That is expected - Task 6 updates it. Within `claude_statusline`, search for other struct literals (e.g. in `wiring.rs` or `tests/real_payload.rs`) and add the two new fields as `None` / asserted values where present:

Run: `cargo build -p claude_statusline --all-targets` and fix any literal that misses the new fields by adding `model_display_name: None, context_used_percent: None,` (or the real values in `tests/real_payload.rs`, which should now assert `Some("Opus 4.7 (1M context)")` and `Some(83.0)`).

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo nextest run -p claude_statusline`
Expected: PASS (new + existing).

- [ ] **Step 7: Lint + commit**

Run: `cargo clippy -p claude_statusline --all-targets -- -D warnings`, then `cargo fmt -p claude_statusline`.

```bash
git add crates/claude_statusline/
git commit -m "feat(statusline): parse model.display_name + context_window.used_percentage"
```

---

### Task 4: Renderer core - segments, layout, countdown, pace (plain text)

**Goal:** Implement `statusline_render::render` producing the configured lines in plain text: model, context_bar, cost, usage (5h/7d with reset countdown + pace annotation). Coloring is stubbed via a single `paint` seam (Task 5 fills it). Cross-provider placeholders (`agent`, `codex`, `openai_cost`) render empty.

**Files:**
- Create: `crates/statusline_render/src/render.rs`
- Modify: `crates/statusline_render/src/lib.rs` (declare + re-export `render`)

**Acceptance Criteria:**
- [ ] For a snapshot with model "Opus", 5h 82% (resets in 1h23m, over pace), 7d 88%, cost $2.50, the rendered output (color=false) contains `🤖 Opus`, `5h 82%`, `(1h23m)`, an `↑` pace arrow, `7d 88%`, and `💰 $2.50`.
- [ ] Absent segments (no model, no 7d window, `cross=None`) are dropped and surrounding whitespace collapses - no empty `{}` artifacts, no double spaces, no blank lines.
- [ ] `render` is pure: same `(snapshot, config, now)` -> same string.

**Verify:** `cargo nextest run -p statusline_render` -> all green; `cargo clippy -p statusline_render --all-targets -- -D warnings` -> clean.

**Steps:**

- [ ] **Step 1: Write failing tests in `crates/statusline_render/src/render.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn cfg() -> settings::StatuslineConfig {
        settings::StatuslineConfig::default()
    }

    // now = 2026-01-01 00:00:00 UTC; 5h resets 1h23m later, 7d resets ~5d later.
    fn now() -> chrono::DateTime<Utc> {
        Utc.timestamp_opt(1_767_225_600, 0).single().unwrap()
    }

    fn snap() -> claude_statusline::StatuslineSnapshot {
        claude_statusline::StatuslineSnapshot {
            rate_limits: Some(claude_statusline::RateLimits {
                five_hour: Some(claude_statusline::RateWindow {
                    used_percent: 82.0,
                    resets_at: now() + chrono::Duration::minutes(83),
                }),
                seven_day: Some(claude_statusline::RateWindow {
                    used_percent: 88.0,
                    resets_at: now() + chrono::Duration::days(5),
                }),
            }),
            session_cost_micro_usd: Some(2_500_000),
            claude_code_version: None,
            model_display_name: Some("Opus".to_string()),
            context_used_percent: Some(42.0),
        }
    }

    #[test]
    fn renders_default_layout_plain() {
        let c = cfg();
        let s = snap();
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: false,
        });
        assert!(out.contains("🤖 Opus"), "model: {out}");
        assert!(out.contains("5h 82%"), "5h pct: {out}");
        assert!(out.contains("(1h23m)"), "5h countdown: {out}");
        assert!(out.contains('↑'), "pace arrow over pace: {out}");
        assert!(out.contains("7d 88%"), "7d pct: {out}");
        assert!(out.contains("💰 $2.50"), "cost: {out}");
        // Cross-provider absent in PR1 -> no codex/openai text.
        assert!(!out.contains("Codex"), "codex empty in PR1: {out}");
        assert!(!out.contains("OpenAI"), "openai empty in PR1: {out}");
    }

    #[test]
    fn absent_segments_collapse_no_artifacts() {
        let c = cfg();
        let s = claude_statusline::StatuslineSnapshot {
            rate_limits: None,
            session_cost_micro_usd: None,
            claude_code_version: None,
            model_display_name: None,
            context_used_percent: None,
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: false,
        });
        assert!(!out.contains('{'), "no unfilled placeholders: {out:?}");
        assert!(!out.contains("  "), "no double spaces: {out:?}");
        assert!(!out.contains("\n\n"), "no blank lines: {out:?}");
    }

    #[test]
    fn render_is_pure() {
        let c = cfg();
        let s = snap();
        let mk = || {
            render(&RenderInput {
                snapshot: &s,
                cross: None,
                config: &c,
                now: now(),
                color: false,
            })
        };
        assert_eq!(mk(), mk());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p statusline_render`
Expected: FAIL to compile (`render`, `RenderInput`, `CrossProvider` undefined).

- [ ] **Step 3: Implement the renderer in `crates/statusline_render/src/render.rs`** (above the test module)

```rust
use chrono::{DateTime, Duration, Utc};

use crate::style::apply_style;

/// Cross-provider data for the statusline. Populated from the watcher snapshot
/// or self-compose in later PRs; `None` in PR1 (placeholders render empty).
#[derive(Debug, Clone, Default)]
pub struct CrossProvider {
    pub codex_used_percent: Option<f32>,
    pub openai_cost_micro_usd: Option<i64>,
    /// True when this cross-provider data is stale (drives the staleness mark).
    pub stale: bool,
}

/// Everything `render` needs. Borrowed; `render` is pure and allocates only the
/// output string.
pub struct RenderInput<'a> {
    pub snapshot: &'a claude_statusline::StatuslineSnapshot,
    pub cross: Option<&'a CrossProvider>,
    pub config: &'a settings::StatuslineConfig,
    pub now: DateTime<Utc>,
    /// Emit ANSI color. The CLI sets this from NO_COLOR only (NOT TTY
    /// detection): Claude Code captures statusline stdout and renders ANSI even
    /// though it is not a terminal.
    pub color: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Base,
    Warn,
    Critical,
}

/// Render the configured lines. Empty segments are dropped and whitespace
/// collapsed; empty lines are omitted; lines join with `\n`.
pub fn render(input: &RenderInput) -> String {
    input
        .config
        .lines
        .iter()
        .map(|tmpl| fill_line(tmpl, input))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Substitute `{key}` placeholders in one template. Each token is either a
/// `{segment}` placeholder (replaced by its rendered value, dropped if empty)
/// or literal text (kept). Segment values may contain spaces - they are
/// inserted whole, so internal spacing is preserved while inter-segment gaps
/// collapse to one space.
fn fill_line(template: &str, input: &RenderInput) -> String {
    let mut parts: Vec<String> = Vec::new();
    for tok in template.split_whitespace() {
        if let Some(key) = tok.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
            if let Some(v) = render_segment(key, input) {
                if !v.is_empty() {
                    parts.push(v);
                }
            }
            // Unknown key or empty value -> drop the token.
        } else {
            parts.push(tok.to_string());
        }
    }
    parts.join(" ")
}

/// Render a single segment by key. `None` -> the segment is omitted entirely.
fn render_segment(key: &str, input: &RenderInput) -> Option<String> {
    let snap = input.snapshot;
    let segs = &input.config.segments;
    match key {
        "model" => {
            let name = snap.model_display_name.as_deref()?;
            Some(paint(
                &format!("🤖 {name}"),
                &segs.model.style,
                "",
                "",
                Tone::Base,
                input.color,
            ))
        }
        // agent parsing is deferred (no `agent` field in a normal payload).
        "agent" => None,
        "context_bar" => {
            let pct = snap.context_used_percent?;
            let c = &segs.context_bar;
            let tone = tone_pct(pct, c.warn, c.critical);
            let text = format!("{} {:.0}%", bar(pct, c.width), pct);
            Some(paint(
                &text,
                &c.style,
                &c.warn_style,
                &c.critical_style,
                tone,
                input.color,
            ))
        }
        "cost" => {
            let micro = snap.session_cost_micro_usd?;
            let c = &segs.cost;
            let tone = tone_money(micro, c.warn_micro_usd, c.critical_micro_usd);
            Some(paint(
                &format!("💰 {}", fmt_money(micro)),
                &c.style,
                &c.warn_style,
                &c.critical_style,
                tone,
                input.color,
            ))
        }
        "usage" => render_usage(input),
        "codex" => {
            let cross = input.cross?;
            let pct = cross.codex_used_percent?;
            let c = &segs.codex;
            let tone = tone_pct(pct, c.warn, c.critical);
            let mark = if cross.stale { " ⚠" } else { "" };
            Some(paint(
                &format!("◇Codex {pct:.0}%{mark}"),
                &c.style,
                &c.warn_style,
                &c.critical_style,
                tone,
                input.color,
            ))
        }
        "openai_cost" => {
            let cross = input.cross?;
            let micro = cross.openai_cost_micro_usd?;
            let mark = if cross.stale { " ⚠" } else { "" };
            Some(paint(
                &format!("OpenAI {}{mark}", fmt_money(micro)),
                &segs.openai_cost.style,
                "",
                "",
                Tone::Base,
                input.color,
            ))
        }
        _ => None,
    }
}

/// Render the 5h + 7d windows as one segment, each window independently toned.
fn render_usage(input: &RenderInput) -> Option<String> {
    let rl = input.snapshot.rate_limits.as_ref()?;
    let c = &input.config.segments.usage;
    let mut windows: Vec<String> = Vec::new();
    if let Some(w) = &rl.five_hour {
        windows.push(render_window("⌛5h", w, Duration::hours(5), c, input));
    }
    if let Some(w) = &rl.seven_day {
        windows.push(render_window("📅7d", w, Duration::days(7), c, input));
    }
    if windows.is_empty() {
        None
    } else {
        Some(windows.join(" "))
    }
}

fn render_window(
    label: &str,
    w: &claude_statusline::RateWindow,
    window_len: Duration,
    c: &settings::statusline::UsageSegment,
    input: &RenderInput,
) -> String {
    let tone = tone_pct(w.used_percent, c.warn, c.critical);
    let mut text = format!("{label} {:.0}%", w.used_percent);
    if c.show_pace {
        let p = window::pace(
            w.used_percent as f64,
            w.resets_at,
            window_len,
            input.now,
        );
        if let Some(ratio) = p.ratio {
            let arrow = if ratio >= 1.0 { '↑' } else { '↓' };
            text.push_str(&format!(" {arrow}{ratio:.1}×"));
        }
    }
    if c.show_reset {
        let delta = w.resets_at - input.now;
        text.push_str(&format!(" ({})", fmt_countdown(delta)));
    }
    paint(
        &text,
        &c.style,
        &c.warn_style,
        &c.critical_style,
        tone,
        input.color,
    )
}

/// Style seam. Task 5 replaces the body with real ANSI application; in Task 4
/// it returns the text unchanged so the layout/text can be tested in isolation.
fn paint(text: &str, base: &str, warn: &str, crit: &str, tone: Tone, color: bool) -> String {
    let _ = (base, warn, crit, tone, color, apply_style as fn(&str, &str) -> String);
    text.to_string()
}

fn tone_pct(pct: f32, warn: u32, critical: u32) -> Tone {
    let p = pct.round() as i64;
    if p >= critical as i64 {
        Tone::Critical
    } else if p >= warn as i64 {
        Tone::Warn
    } else {
        Tone::Base
    }
}

fn tone_money(micro: i64, warn: i64, critical: i64) -> Tone {
    if micro >= critical {
        Tone::Critical
    } else if micro >= warn {
        Tone::Warn
    } else {
        Tone::Base
    }
}

/// micro-USD -> "$X.XX". f64 only at this display boundary (AGENTS.md §2.1).
fn fmt_money(micro: i64) -> String {
    format!("${:.2}", micro as f64 / 1_000_000.0)
}

/// ASCII utilization bar of `width` cells, e.g. "[####------]".
fn bar(pct: f32, width: u32) -> String {
    let w = width.max(1);
    let filled = ((pct / 100.0) * w as f32).round().clamp(0.0, w as f32) as u32;
    let empty = w - filled;
    format!(
        "[{}{}]",
        "#".repeat(filled as usize),
        "-".repeat(empty as usize)
    )
}

/// Compact reset countdown: "1h23m", "3d4h", "12m". Past/zero -> "0m".
fn fmt_countdown(delta: Duration) -> String {
    let secs = delta.num_seconds().max(0);
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m}m")
    } else {
        format!("{m}m")
    }
}
```

- [ ] **Step 4: Declare the module in `crates/statusline_render/src/lib.rs`**

```rust
pub mod style;
mod render;
pub use render::{CrossProvider, RenderInput, render};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p statusline_render`
Expected: PASS (the three render tests + the style tests).

- [ ] **Step 6: Lint + commit**

Run: `cargo clippy -p statusline_render --all-targets -- -D warnings`, then `cargo fmt -p statusline_render`.

```bash
git add crates/statusline_render/
git commit -m "feat(statusline): render segments, layout, reset countdown + pace"
```

---

### Task 5: Threshold coloring via the style seam

**Goal:** Fill the `paint` seam so segments emit per-tone ANSI from config (base/warn/critical styles), gated by the `color` flag. No other renderer change.

**Files:**
- Modify: `crates/statusline_render/src/render.rs` (the `paint` fn + tests)

**Acceptance Criteria:**
- [ ] With `color=false`, output is byte-identical to Task 4 (no escapes anywhere).
- [ ] With `color=true`, a critical-tone segment (e.g. 7d 88% with critical=90 -> Warn; 5h 95% -> Critical) is wrapped in the configured `critical_style` ANSI; a base-tone segment uses `style`; an empty style string yields no escapes even with `color=true`.
- [ ] A warn-tone segment whose `warn_style` is blank falls back to no escapes (blank style = unstyled), not a panic.

**Verify:** `cargo nextest run -p statusline_render` -> all green; `cargo clippy -p statusline_render --all-targets -- -D warnings` -> clean.

**Steps:**

- [ ] **Step 1: Write failing tests in `crates/statusline_render/src/render.rs`** (append to `tests`)

```rust
    #[test]
    fn color_false_has_no_escapes() {
        let c = cfg();
        let s = snap();
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: false,
        });
        assert!(!out.contains('\x1b'), "no ANSI when color=false: {out:?}");
    }

    #[test]
    fn color_true_wraps_toned_segments() {
        let c = cfg();
        // 5h at 95% -> critical (>=90); default critical_style = "bold fg:#f7768e".
        let mut s = snap();
        s.rate_limits.as_mut().unwrap().five_hour.as_mut().unwrap().used_percent = 95.0;
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: true,
        });
        assert!(out.contains('\x1b'), "ANSI present when color=true: {out:?}");
        // bold + truecolor #f7768e = rgb(247,118,142)
        assert!(
            out.contains("\x1b[1;38;2;247;118;142m"),
            "critical style applied to 5h: {out:?}"
        );
    }

    #[test]
    fn blank_style_yields_no_escapes_even_with_color() {
        let mut c = cfg();
        c.segments.cost.style = String::new();
        c.segments.cost.warn_style = String::new();
        c.segments.cost.critical_style = String::new();
        let s = snap(); // cost $2.50 -> warn (>=2_000_000)
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: true,
        });
        assert!(out.contains("💰 $2.50"), "cost text present: {out:?}");
        // The cost substring must not be wrapped (blank warn_style).
        assert!(
            !out.contains("\x1b[") || !out.split("💰").nth(1).unwrap_or("").starts_with('m'),
            "blank style must not wrap cost: {out:?}"
        );
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p statusline_render`
Expected: `color_true_wraps_toned_segments` FAILS (the `paint` stub never adds escapes).

- [ ] **Step 3: Replace the `paint` stub in `crates/statusline_render/src/render.rs`**

```rust
/// Apply the tone's configured style to `text`, gated by `color`. A blank style
/// string (or `color=false`) returns the text unchanged.
fn paint(text: &str, base: &str, warn: &str, crit: &str, tone: Tone, color: bool) -> String {
    if !color {
        return text.to_string();
    }
    let spec = match tone {
        Tone::Base => base,
        Tone::Warn => warn,
        Tone::Critical => crit,
    };
    apply_style(spec, text)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p statusline_render`
Expected: PASS (all render + style tests; the Task 4 plain-text tests still hold because they use `color: false`).

- [ ] **Step 5: Lint + commit**

Run: `cargo clippy -p statusline_render --all-targets -- -D warnings`, then `cargo fmt -p statusline_render`.

```bash
git add crates/statusline_render/
git commit -m "feat(statusline): apply per-tone threshold coloring to segments"
```

---

### Task 6: Rewire `balanze-cli statusline` onto the renderer

**Goal:** Replace `format_statusline_from_snapshot` with `statusline_render::render` (load settings, color from NO_COLOR only, fixed-now via `Utc::now()`, `cross=None`), keep the IPC snapshot write and the frozen no-arg contract, and update the crate's tests.

**Files:**
- Modify: `crates/balanze_cli/Cargo.toml` (add `statusline_render` dep)
- Modify: `crates/balanze_cli/src/statusline.rs` (render path + tests)

**Acceptance Criteria:**
- [ ] `balanze-cli statusline` fed a known payload on stdin prints the new multi-line rendered output and still writes `statusline.snapshot.json` (snapshot-write tests pass).
- [ ] Color is emitted unless `NO_COLOR` is set (it does NOT depend on TTY detection, because Claude Code invokes the bare command with piped stdout and renders ANSI).
- [ ] A parse error still prints a non-empty fallback line and does not panic.
- [ ] Removed exact-string format tests are superseded by the deterministic `statusline_render` renderer tests (no coverage loss).

**Verify:** `cargo nextest run -p balanze_cli` -> all green; `cargo clippy -p balanze_cli --all-targets -- -D warnings` -> clean; manual: `echo '{"rate_limits":{"five_hour":{"used_percentage":82,"resets_at":4102444800}},"cost":{"total_cost_usd":2.5},"model":{"display_name":"Opus"}}' | cargo run -p balanze_cli -- statusline`.

**Steps:**

- [ ] **Step 1: Add the dependency to `crates/balanze_cli/Cargo.toml`**

Under `[dependencies]`:

```toml
statusline_render = { path = "../statusline_render" }
```

- [ ] **Step 2: Rewrite `cmd_statusline` + helpers in `crates/balanze_cli/src/statusline.rs`**

Replace `cmd_statusline`, `format_statusline_from_snapshot`, and the `#[cfg(test)] fn format_statusline` with:

```rust
pub(crate) fn cmd_statusline() -> Result<()> {
    use std::io::Read as _;
    let mut stdout = std::io::stdout().lock();
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        let _ = writeln!(stdout, "bal (statusline: stdin unreadable)");
        return Ok(());
    }
    let snap = match claude_statusline::parse(&buf) {
        Ok(s) => s,
        Err(_) => {
            let _ = writeln!(stdout, "bal (statusline parse error)");
            return Ok(());
        }
    };
    let _ = writeln!(stdout, "{}", render_line(&snap));
    write_statusline_snapshot(&snap);
    Ok(())
}

/// Render the configured statusline for `snap`. Settings load failure falls
/// back to the curated default (the statusline must never fail to render).
/// Color is gated on `NO_COLOR` only - Claude Code captures stdout (not a TTY)
/// and renders ANSI, so TTY detection would wrongly strip color.
fn render_line(snap: &claude_statusline::StatuslineSnapshot) -> String {
    let settings = settings::load().unwrap_or_default();
    let color = std::env::var_os("NO_COLOR").is_none();
    statusline_render::render(&statusline_render::RenderInput {
        snapshot: snap,
        cross: None,
        config: &settings.statusline,
        now: chrono::Utc::now(),
        color,
    })
}
```

- [ ] **Step 3: Update the test module in `crates/balanze_cli/src/statusline.rs`**

Remove the five exact-string tests (`formats_full_payload`, `formats_no_rate_limits`, `formats_empty_payload`, `parse_error_is_nonempty_fallback_not_panic`, `formats_only_seven_day`) and the `use super::format_statusline;` import - they pinned the old single-line format, now superseded by the deterministic renderer tests in `statusline_render` (which inject a fixed `now`; the CLI cannot, since it calls `Utc::now()`). Keep `EnvGuard`, `statusline_snapshot_path_honors_env_override`, and `write_statusline_snapshot_lands_at_data_dir_override`. Add the two new `StatuslineSnapshot` fields to the literal in the latter:

```rust
        let snap = StatuslineSnapshot {
            rate_limits: None,
            session_cost_micro_usd: Some(3_420_000),
            claude_code_version: Some("v2.1.144".to_string()),
            model_display_name: None,
            context_used_percent: None,
        };
```

Add one render smoke test (no exact-now assertions):

```rust
    #[test]
    fn render_line_smoke_contains_known_segments() {
        let snap = claude_statusline::parse(
            r#"{"rate_limits":{"five_hour":{"used_percentage":82,"resets_at":4102444800}},"cost":{"total_cost_usd":2.5},"model":{"display_name":"Opus"}}"#,
        )
        .unwrap();
        // NO_COLOR for deterministic, escape-free assertion.
        let _g = EnvGuard::set("NO_COLOR", "1");
        let out = super::render_line(&snap);
        assert!(out.contains("🤖 Opus"), "{out}");
        assert!(out.contains("5h 82%"), "{out}");
        assert!(out.contains("💰 $2.50"), "{out}");
    }
```

(Note: `render_line` reads the user's real `settings.json`. The smoke test only asserts substrings that the curated default lines always include - it does not assert the full line, so a customized local config cannot break it. If a hermetic test is later needed, factor `render_line` to take a `&StatuslineConfig`.)

- [ ] **Step 4: Run the workspace build + tests**

Run: `cargo build --workspace` then `cargo nextest run -p balanze_cli -p statusline_render -p settings -p claude_statusline`
Expected: PASS.

- [ ] **Step 5: Manual smoke**

Run:
```bash
echo '{"rate_limits":{"five_hour":{"used_percentage":82,"resets_at":4102444800},"seven_day":{"used_percentage":88,"resets_at":4102444800}},"cost":{"total_cost_usd":2.5},"model":{"display_name":"Opus 4.7"},"context_window":{"used_percentage":42}}' | cargo run -p balanze_cli -- statusline
```
Expected: two colored lines, line 1 `🤖 Opus 4.7`, line 2 `[####------] 42% 💰 $2.50 ⌛5h 82% ... 📅7d 88% ...`. Re-run with `NO_COLOR=1` to confirm escapes drop.

- [ ] **Step 6: Lint + commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings` (the always-on CI gate excludes `src-tauri`; this PR does not touch it), then `cargo fmt --all`.

```bash
git add crates/balanze_cli/
git commit -m "feat(statusline): render configurable colored line in balanze-cli statusline"
```

---

## Self-Review

**Spec coverage (PR1 scope only):**
- New `statusline_render` crate (spec §3.1) -> Tasks 1, 4, 5.
- Config in `settings.json` (spec §4.3, D5) -> Task 2.
- Parser extension (spec §3.2) -> Task 3 (model + context; agent/effort/output_style deferred per scope note).
- Segment model + layout + default-on reset countdown + pace (spec §4, D6) -> Tasks 4, 5.
- Threshold coloring, config-driven, not reusing the 50/90 buckets (spec §4.1, §8) -> Tasks 2 (integer thresholds) + 5.
- Color gating from NO_COLOR not TTY (spec §3.3 zero-auth stdin path) -> Task 6.
- Out of PR1 scope (later PRs): cross-provider data + Hybrid (PR2), cache (PR3), replace flow (PR4), Codex preset + docs (PR5), staleness marker live data (PR2 - the rendering hook exists in Task 4 via `CrossProvider.stale`).

**Placeholder scan:** none - every code/test block is complete; no "TODO"/"similar to"/"add error handling".

**Type consistency:** `StatuslineSnapshot` gains `model_display_name`/`context_used_percent` (Task 3) and is constructed with them in Tasks 4 (tests) and 6 (CLI literal). `RenderInput`/`CrossProvider`/`render` defined in Task 4, consumed in Task 6. `settings::StatuslineConfig` + `settings::statusline::UsageSegment` defined Task 2, referenced in Task 4 (`render_window` param) and Task 6. `paint` signature identical across Tasks 4 and 5. `apply_style` defined Task 1, used in Task 5.

**Known simplifications (intentional, noted in code):** money formatter duplicated minimally rather than sharing `balanze_cli::format::micro_usd_to_display_dollars` (cross-dep would invert the crate direction); ASCII context bar (portability); inter-segment whitespace collapses to single space.
