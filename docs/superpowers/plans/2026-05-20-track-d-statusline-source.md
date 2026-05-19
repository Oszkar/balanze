# Track D — Claude Code statusline source Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ingest Claude Code's `statusLine` stdin payload as a first-class, schema-owned source — a new `claude_statusline` crate that parses it, a `balanze-cli statusline` subcommand Claude Code can call, and a `setup` step that wires it — without touching the Snapshot/compose/coordinator (that integration is Track E).

**Architecture:** New `claude_statusline` crate is the ONLY code that knows the statusLine wire format AND owns the `statusLine` stanza in Claude Code's `settings.json` (read+safe-write) — exactly mirroring how `anthropic_oauth` owns both reading and writing `~/.claude/.credentials.json` (AGENTS.md §4 precedent). It is pure parse + careful fs; it does NOT import `reqwest`, `tokio`, `snapshot_composer`, or `state_coordinator`. `balanze_cli` is the only consumer: a fast, never-failing `statusline` subcommand (Claude Code's contract: JSON on stdin → one status line on stdout) and a `setup` wiring step. The parsed `StatuslineSnapshot` is exposed but NOT fed into `compose()`/the coordinator here — the layered statusline-primary/OAuth-fallback live model is Track E's redefined-watcher work. Drawing the D/E line here keeps Track D tight and independently shippable: after Track D, the source exists, parses, and works as a Claude Code statusLine command; Track E makes it drive the live Snapshot.

**Tech Stack:** Rust 2021, Cargo workspace (`crates/*` glob auto-includes the new member — no root `Cargo.toml` edit). `serde`/`serde_json`/`chrono`/`thiserror` (workspace deps), `tempfile` (dev). Gates: `cargo fmt`/`clippy --workspace --all-targets -D warnings`/`test --workspace` + `bun run check`; Conventional Commits (blocking `commit-msg` hook); never `--no-verify`.

**Documented payload schema (authoritative, from the spike `~/.gstack/projects/balanze/spike-statusline-payload-20260519.md`):** stdin JSON includes `rate_limits.{five_hour,seven_day}.{used_percentage:number 0-100, resets_at:number unix-epoch-seconds}` and `cost.total_cost_usd:number` (dollars; a Claude-side **session estimate**), plus `model`, `version`, `workspace`, `context_window.*`, etc. **`rate_limits` is present only for Pro/Max and only after the first API response in a session — absent must be a clean `None`, never an error.** The payload schema evolves (`context_window.*` changed at v2.1.132) → tolerate unknown/missing fields like `claude_parser` does.

**AGENTS.md compliance:** §4 — new boundary: only `claude_statusline` knows the statusLine wire format + owns the `statusLine` key in Claude `settings.json`; `balanze_cli` consumes the typed struct, encodes no wire knowledge. §2.1 — currency i64 micro-USD: `cost.total_cost_usd` (f64 dollars on the wire) converts to i64 micro-USD at the crate boundary, never float money math downstream. §3.4-adjacent — `settings.json` is not secret but is the user's Claude Code config: writes are atomic (tmp+fsync+rename), preserve every other key, idempotent, reversible, and **asked before** (§0 high-impact: editing the user's settings.json). §8 — new crate (Repo Map + boundary entry), new subcommand surface, new settings.json write surface; doc-sync task included. NO Snapshot/`UsageEvent`/`compose()` change (Track E owns that).

---

### Task 1: `claude_statusline` crate — parse the statusLine payload (TDD)

**Goal:** A new pure crate that deserializes the Claude Code statusLine stdin JSON into a typed `StatuslineSnapshot`, with `claude_parser`-grade schema-drift discipline.

**Files:**
- Create: `crates/claude_statusline/Cargo.toml`
- Create: `crates/claude_statusline/src/lib.rs`
- Create: `crates/claude_statusline/src/errors.rs`
- Create: `crates/claude_statusline/src/types.rs`
- Create: `crates/claude_statusline/src/parse.rs`

**Acceptance Criteria:**
- [ ] `claude_statusline::parse(&str) -> Result<StatuslineSnapshot, StatuslineError>` exists and is pure (no I/O, no `tokio`/`reqwest`).
- [ ] A full Pro/Max payload yields `rate_limits = Some` with `five_hour`/`seven_day` `RateWindow { used_percent: f32, resets_at: DateTime<Utc> }` (epoch-seconds → UTC) and `session_cost_micro_usd = Some(i64)` (`total_cost_usd` dollars ×1_000_000, round-half-away-from-zero, saturating).
- [ ] Missing `rate_limits` (non-Pro/Max or pre-first-response) → `rate_limits: None`, NOT an error. Missing `cost` → `session_cost_micro_usd: None`.
- [ ] Unknown/extra top-level + nested fields are tolerated (no error). Invalid JSON → `StatuslineError::InvalidJson`. A present-but-wrong-type required subfield (e.g. `rate_limits.five_hour.resets_at` is a string) → `StatuslineError::SchemaDrift { message }`.
- [ ] `claude_code_version: Option<String>` captured (from top-level `version`) for drift diagnostics.
- [ ] `cargo test -p claude_statusline` green; `cargo clippy -p claude_statusline --all-targets -- -D warnings` clean.

**Verify:** `cargo test -p claude_statusline && cargo clippy -p claude_statusline --all-targets -- -D warnings && cargo fmt -p claude_statusline -- --check`

**Steps:**

- [ ] **Step 1: Create `crates/claude_statusline/Cargo.toml`** (mirrors `crates/codex_local/Cargo.toml`):

```toml
[package]
name = "claude_statusline"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
publish.workspace = true
description = "Parses the Claude Code statusLine stdin payload and owns the statusLine stanza in Claude Code's settings.json. The only code that knows that wire format."

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
anyhow = { workspace = true }
tempfile = "3"
```

- [ ] **Step 2: Write the failing tests** in `crates/claude_statusline/src/parse.rs` (`#[cfg(test)] mod tests`). Include: full Pro/Max payload (assert rate_limits Some, both windows, cost micro), payload without `rate_limits` (None, not error), payload without `cost` (None), invalid JSON (`InvalidJson`), `rate_limits.five_hour.resets_at` as a string (`SchemaDrift`), extra unknown fields tolerated, epoch→UTC correctness, dollars→micro rounding (`1.2345675` → `1_234_568`). Use this fixture body in the happy-path test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{
      "version":"2.1.140","model":{"id":"claude-opus-4-7","display_name":"Opus"},
      "workspace":{"current_dir":"/x","project_dir":"/x"},
      "cost":{"total_cost_usd":12.5,"total_duration_ms":1000,"total_lines_added":3},
      "context_window":{"total_input_tokens":1,"used_percentage":4.2},
      "rate_limits":{
        "five_hour":{"used_percentage":13.0,"resets_at":1747650600},
        "seven_day":{"used_percentage":44.0,"resets_at":1747915200}
      }}"#;

    #[test]
    fn parses_full_pro_max_payload() {
        let s = parse(FULL).expect("parses");
        let rl = s.rate_limits.expect("rate_limits present");
        let fh = rl.five_hour.expect("five_hour");
        assert!((fh.used_percent - 13.0).abs() < 1e-4);
        assert_eq!(fh.resets_at.timestamp(), 1747650600);
        assert!((rl.seven_day.unwrap().used_percent - 44.0).abs() < 1e-4);
        // $12.50 → 12_500_000 micro-USD
        assert_eq!(s.session_cost_micro_usd, Some(12_500_000));
        assert_eq!(s.claude_code_version.as_deref(), Some("2.1.140"));
    }

    #[test]
    fn missing_rate_limits_is_none_not_error() {
        let body = r#"{"version":"2.1.140","cost":{"total_cost_usd":1.0}}"#;
        let s = parse(body).expect("parses without rate_limits");
        assert!(s.rate_limits.is_none());
        assert_eq!(s.session_cost_micro_usd, Some(1_000_000));
    }

    #[test]
    fn missing_cost_is_none() {
        let s = parse(r#"{"version":"2.1.140"}"#).expect("parses");
        assert!(s.session_cost_micro_usd.is_none());
        assert!(s.rate_limits.is_none());
    }

    #[test]
    fn invalid_json_is_invalid_json_error() {
        match parse("{not json") {
            Err(StatuslineError::InvalidJson(_)) => {}
            other => panic!("expected InvalidJson, got {other:?}"),
        }
    }

    #[test]
    fn wrong_type_required_subfield_is_schema_drift() {
        let body = r#"{"rate_limits":{"five_hour":{"used_percentage":1.0,"resets_at":"soon"}}}"#;
        match parse(body) {
            Err(StatuslineError::SchemaDrift { .. }) => {}
            other => panic!("expected SchemaDrift, got {other:?}"),
        }
    }

    #[test]
    fn unknown_fields_tolerated_and_dollars_round_half_away() {
        let body = r#"{"brand_new_field":42,"cost":{"total_cost_usd":1.2345675}}"#;
        let s = parse(body).expect("tolerates unknown fields");
        assert_eq!(s.session_cost_micro_usd, Some(1_234_568)); // round half away
    }

    #[test]
    fn one_window_present_other_absent() {
        let body = r#"{"rate_limits":{"five_hour":{"used_percentage":9.0,"resets_at":1747650600}}}"#;
        let rl = parse(body).unwrap().rate_limits.unwrap();
        assert!(rl.five_hour.is_some());
        assert!(rl.seven_day.is_none());
    }
}
```

- [ ] **Step 3: Run tests, expect failure** (`parse`/types not defined yet):
Run: `cargo test -p claude_statusline` → Expected: compile error / FAIL.

- [ ] **Step 4: `crates/claude_statusline/src/errors.rs`:**

```rust
use thiserror::Error;

/// Errors from parsing the statusLine payload OR managing the statusLine
/// stanza in Claude Code's settings.json. One enum (mirrors
/// `anthropic_oauth::OAuthError`'s single-enum approach).
#[derive(Debug, Error)]
pub enum StatuslineError {
    #[error("statusline payload is not valid JSON: {0}")]
    InvalidJson(String),

    #[error("statusline payload schema drift: {message}")]
    SchemaDrift { message: String },

    #[error("Claude settings.json not found (looked at {searched:?})")]
    SettingsMissing { searched: Vec<std::path::PathBuf> },

    #[error("io error on {path:?}: {source}")]
    SettingsIo {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Claude settings.json at {path:?} is malformed: {reason}")]
    SettingsMalformed {
        path: std::path::PathBuf,
        reason: String,
    },
}
```

- [ ] **Step 5: `crates/claude_statusline/src/types.rs`:**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One server-authoritative subscription window from the statusLine feed.
/// Field shapes mirror `anthropic_oauth::CadenceBar` (`used_percent` f32,
/// `resets_at` DateTime<Utc>) so Track E can treat the two sources uniformly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateWindow {
    pub used_percent: f32,
    pub resets_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimits {
    pub five_hour: Option<RateWindow>,
    pub seven_day: Option<RateWindow>,
}

/// Parsed statusLine payload. `None` fields = "not present in this payload"
/// (e.g. `rate_limits` is Pro/Max-only and only after the first API
/// response). `session_cost_micro_usd` is a Claude-side SESSION ESTIMATE
/// (i64 micro-USD, AGENTS.md §2.1) — a distinct cost tier, never conflated
/// with the JSONL list-price estimate or the real `extra_usage` overage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatuslineSnapshot {
    pub rate_limits: Option<RateLimits>,
    pub session_cost_micro_usd: Option<i64>,
    pub claude_code_version: Option<String>,
}
```

- [ ] **Step 6: `crates/claude_statusline/src/parse.rs`** (above the test mod):

```rust
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;

use crate::errors::StatuslineError;
use crate::types::{RateLimits, RateWindow, StatuslineSnapshot};

#[derive(Debug, Deserialize)]
struct RawRoot {
    version: Option<String>,
    cost: Option<RawCost>,
    rate_limits: Option<RawRateLimits>,
}

#[derive(Debug, Deserialize)]
struct RawCost {
    #[serde(default)]
    total_cost_usd: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawRateLimits {
    five_hour: Option<RawWindow>,
    seven_day: Option<RawWindow>,
}

#[derive(Debug, Deserialize)]
struct RawWindow {
    used_percentage: f32,
    /// Unix epoch SECONDS (per the documented schema).
    resets_at: i64,
}

/// Parse the Claude Code statusLine stdin payload. Pure, infallible except
/// for invalid JSON or a present-but-wrong-shape required subfield. Unknown
/// fields are tolerated; absent optional blocks become `None`.
pub fn parse(input: &str) -> Result<StatuslineSnapshot, StatuslineError> {
    let raw: RawRoot = serde_json::from_str(input).map_err(|e| {
        // serde's classify distinguishes syntax (invalid JSON) from a
        // type/shape mismatch on a field we declared (schema drift).
        match e.classify() {
            serde_json::error::Category::Data => StatuslineError::SchemaDrift {
                message: e.to_string(),
            },
            _ => StatuslineError::InvalidJson(e.to_string()),
        }
    })?;

    let session_cost_micro_usd = raw
        .cost
        .and_then(|c| c.total_cost_usd)
        .map(usd_to_micro);

    let rate_limits = raw.rate_limits.map(|rl| RateLimits {
        five_hour: rl.five_hour.map(to_window),
        seven_day: rl.seven_day.map(to_window),
    });

    Ok(StatuslineSnapshot {
        rate_limits,
        session_cost_micro_usd,
        claude_code_version: raw.version,
    })
}

fn to_window(w: RawWindow) -> RateWindow {
    // resets_at is epoch seconds; clamp to a valid timestamp (epoch on the
    // pathological out-of-range case rather than panic).
    let resets_at: DateTime<Utc> = Utc
        .timestamp_opt(w.resets_at, 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap());
    RateWindow {
        used_percent: w.used_percentage,
        resets_at,
    }
}

/// Dollars (f64, from the wire) → i64 micro-USD, round half away from zero,
/// saturating. This is the ONLY f64→money conversion; everything downstream
/// is i64 micro-USD (AGENTS.md §2.1).
fn usd_to_micro(usd: f64) -> i64 {
    let scaled = usd * 1_000_000.0;
    let rounded = scaled.round(); // f64::round is half-away-from-zero
    if rounded >= i64::MAX as f64 {
        i64::MAX
    } else if rounded <= i64::MIN as f64 {
        i64::MIN
    } else {
        rounded as i64
    }
}
```

- [ ] **Step 7: `crates/claude_statusline/src/lib.rs`:**

```rust
//! Parses the Claude Code `statusLine` stdin payload and owns the
//! `statusLine` stanza in Claude Code's `settings.json`.
//!
//! Sits in the schema-owning data-source tier alongside `claude_parser`
//! (§4 #1) and `codex_local` (§4 #11): it is the ONLY code that knows the
//! statusLine wire format, and — mirroring `anthropic_oauth` for
//! `.credentials.json` — also the only code that reads/writes the
//! `statusLine` key in Claude's `settings.json`.
//!
//! `rate_limits` is Pro/Max-only and only present after the first API
//! response in a session; absent is `None`, never an error. The payload
//! schema evolves (e.g. `context_window.*` at v2.1.132) so unknown/missing
//! fields are tolerated. Track E (not this crate) wires the parsed snapshot
//! into the live Snapshot/coordinator.

pub mod errors;
pub mod parse;
pub mod types;
pub mod wiring;

pub use errors::StatuslineError;
pub use parse::parse;
pub use types::{RateLimits, RateWindow, StatuslineSnapshot};
pub use wiring::{locate_settings_path, read_wire_status, wire_statusline, WireStatus};
```

(`wiring` module is created in Task 4; for Task 1 add `pub mod wiring;` + the `pub use` only after Task 4, OR stub `pub mod wiring {}` now and fill in Task 4. To keep Task 1 self-contained and compiling, OMIT the `wiring` line + its `pub use` in Task 1 and add them in Task 4 Step where noted.)

- [ ] **Step 8: Adjust lib.rs for Task 1 scope** — for THIS task only, `lib.rs` is:

```rust
//! (module doc as above, minus the last sentence about wiring)
pub mod errors;
pub mod parse;
pub mod types;

pub use errors::StatuslineError;
pub use parse::parse;
pub use types::{RateLimits, RateWindow, StatuslineSnapshot};
```

- [ ] **Step 9: Run tests green:**
Run: `cargo test -p claude_statusline` → Expected: all tests PASS.

- [ ] **Step 10: Gates + commit:**
Run: `cargo fmt -p claude_statusline && cargo clippy -p claude_statusline --all-targets -- -D warnings && cargo test -p claude_statusline`
```bash
git add crates/claude_statusline/Cargo.toml crates/claude_statusline/src/
git commit -m "feat(claude_statusline): parse the Claude Code statusLine stdin payload"
```

---

### Task 2: Real captured payload fixture + version confirmation

> **USER-ORDERED GATE — NON-SKIPPABLE.** This task was requested by the user in the current conversation. It MUST NOT be closed by walking around it, by declaring it "verified inline", or by substituting a cheaper check. Close only after every item in `acceptanceCriteria` has been re-validated independently, with output captured.

**Goal:** Replace reliance on the synthetic fixture with a REAL captured statusLine payload from the maintainer's machine, committed as a test fixture, proving the installed Claude Code version actually emits `rate_limits`.

**Files:**
- Create: `crates/claude_statusline/tests/fixtures/real-payload.json` (maintainer-provided, secrets/paths redacted)
- Create: `crates/claude_statusline/tests/real_payload.rs`

**Acceptance Criteria:**
- [ ] A real captured payload (maintainer ran the documented capture: a throwaway `statusLine` command dumping stdin, ≥1 assistant turn, Pro/Max) is committed at `tests/fixtures/real-payload.json` with `cwd`/`transcript_path`/`session_id`/`workspace` paths redacted (replace with `"REDACTED"`), `rate_limits` + `cost` left intact.
- [ ] `tests/real_payload.rs` loads it, calls `claude_statusline::parse`, asserts `rate_limits.is_some()` AND at least `five_hour` is `Some` (proves the installed version emits it), and `session_cost_micro_usd.is_some()`.
- [ ] The captured payload's `version` field is recorded in a comment in the fixture or test (the confirmed-emitting Claude Code version).
- [ ] `cargo test -p claude_statusline --test real_payload` green.

**Verify:** `cargo test -p claude_statusline --test real_payload -- --nocapture`

**Steps:**

- [ ] **Step 1: Obtain the real payload from the maintainer.** It is requested in the conversation (capture recipe already provided). If not yet available, report `NEEDS_CONTEXT` requesting `~/statusline-payload.json` (or pasted JSON). Do NOT fabricate it — a synthetic stand-in defeats the purpose of this task.
- [ ] **Step 2: Redact + commit the fixture.** Replace `cwd`, `transcript_path`, `session_id`, and any `workspace.*`/path strings with `"REDACTED"`; keep `version`, `rate_limits`, `cost`, `model` intact. Save to `crates/claude_statusline/tests/fixtures/real-payload.json`.
- [ ] **Step 3: Write `crates/claude_statusline/tests/real_payload.rs`:**

```rust
//! Pins the REAL statusLine payload shape from the maintainer's installed
//! Claude Code (version recorded in the fixture). Proves the documented
//! `rate_limits` block is actually emitted, not just documented.
use claude_statusline::parse;

#[test]
fn real_captured_payload_parses_with_rate_limits() {
    let body = include_str!("fixtures/real-payload.json");
    let s = parse(body).expect("real payload parses");
    let rl = s
        .rate_limits
        .expect("installed Claude Code version emits rate_limits (Pro/Max, post-first-response)");
    assert!(
        rl.five_hour.is_some(),
        "five_hour window present in the real payload"
    );
    assert!(
        s.session_cost_micro_usd.is_some(),
        "cost.total_cost_usd present in the real payload"
    );
}
```

- [ ] **Step 4: Run + commit:**
Run: `cargo test -p claude_statusline --test real_payload -- --nocapture` → PASS.
```bash
git add crates/claude_statusline/tests/
git commit -m "test(claude_statusline): pin real captured statusLine payload (version-confirmed)"
```

```json:metadata
{"files":["crates/claude_statusline/tests/fixtures/real-payload.json","crates/claude_statusline/tests/real_payload.rs"],"verifyCommand":"cargo test -p claude_statusline --test real_payload -- --nocapture","acceptanceCriteria":["real maintainer-captured payload committed (paths redacted, rate_limits+cost intact)","parse() returns rate_limits Some with five_hour Some and session_cost Some","confirmed Claude Code version recorded"],"userGate":true,"tags":["user-gate"]}
```

---

### Task 3: `balanze-cli statusline` subcommand

**Goal:** A fast, never-failing subcommand Claude Code can call as its `statusLine` command: JSON on stdin → one honest status line on stdout, exit 0 always.

**Files:**
- Modify: `crates/balanze_cli/Cargo.toml` (add the path dep)
- Modify: `crates/balanze_cli/src/main.rs` (dispatch arm, `cmd_statusline`, `print_help`)

**Acceptance Criteria:**
- [ ] `crates/balanze_cli/Cargo.toml` `[dependencies]` gains `claude_statusline = { path = "../claude_statusline" }` (alphabetical with the other path deps).
- [ ] `"statusline" => cmd_statusline()` added to the dispatch `match` in `main()`.
- [ ] `cmd_statusline()` reads all of stdin, calls `claude_statusline::parse`, prints ONE line to **stdout** summarising 5h/7d % + session cost when present (e.g. `bal 5h 13% · 7d 44% · sess $12.50`), a minimal line when fields absent (e.g. `bal (no rate-limit data yet)`), and on parse error prints a non-empty fallback (`bal (statusline parse error)`) — and ALWAYS returns `Ok(())` (exit 0). Never panics, never non-zero (Claude Code renders the output; a crash = broken status line).
- [ ] `print_help()` lists the `statusline` subcommand.
- [ ] Unit tests for the formatter (snapshot → line) cover: both windows + cost; rate_limits None; cost None; cover the `Display`-free path.
- [ ] `cargo test --workspace` green incl. existing `integration_4quadrant.rs` (unchanged — proves no Snapshot drift).

**Verify:** `printf '%s' '{"rate_limits":{"five_hour":{"used_percentage":13,"resets_at":1747650600},"seven_day":{"used_percentage":44,"resets_at":1747915200}},"cost":{"total_cost_usd":12.5}}' | cargo run -q -p balanze_cli -- statusline` → prints one line, exit 0; `echo 'not json' | cargo run -q -p balanze_cli -- statusline; echo "exit=$?"` → fallback line, `exit=0`.

**Steps:**

- [ ] **Step 1: Add the dependency** to `crates/balanze_cli/Cargo.toml` under `[dependencies]`, keeping the path-deps alphabetical (after `claude_parser`):

```toml
claude_parser = { path = "../claude_parser" }
claude_statusline = { path = "../claude_statusline" }
codex_local = { path = "../codex_local" }
```

- [ ] **Step 2: Add a formatter + `cmd_statusline` to `crates/balanze_cli/src/main.rs`** (place `cmd_statusline` near the other `cmd_*` fns; reuse the existing `micro_usd_to_display_dollars` at main.rs:40). Insert:

```rust
fn cmd_statusline() -> Result<()> {
    use std::io::Read as _;
    let mut buf = String::new();
    // stdin read failure → still emit a line + exit 0 (Claude Code renders
    // whatever we print; a crash/non-zero leaves an ugly/empty status line).
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        println!("bal (statusline: stdin unreadable)");
        return Ok(());
    }
    println!("{}", format_statusline(&buf));
    Ok(())
}

/// Pure: payload string → one status-line string. Track D = a minimal
/// honest line. Rich/configurable formatting + feeding the live Snapshot
/// is Track E (the redefined watcher).
fn format_statusline(payload: &str) -> String {
    let snap = match claude_statusline::parse(payload) {
        Ok(s) => s,
        Err(_) => return "bal (statusline parse error)".to_string(),
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(rl) = &snap.rate_limits {
        if let Some(w) = &rl.five_hour {
            parts.push(format!("5h {:.0}%", w.used_percent));
        }
        if let Some(w) = &rl.seven_day {
            parts.push(format!("7d {:.0}%", w.used_percent));
        }
    }
    if let Some(c) = snap.session_cost_micro_usd {
        parts.push(format!("sess {}", micro_usd_to_display_dollars(c)));
    }
    if parts.is_empty() {
        "bal (no rate-limit data yet)".to_string()
    } else {
        format!("bal {}", parts.join(" · "))
    }
}

#[cfg(test)]
mod statusline_tests {
    use super::format_statusline;

    #[test]
    fn formats_full_payload() {
        let p = r#"{"rate_limits":{"five_hour":{"used_percentage":13.0,"resets_at":1747650600},"seven_day":{"used_percentage":44.0,"resets_at":1747915200}},"cost":{"total_cost_usd":12.5}}"#;
        assert_eq!(format_statusline(p), "bal 5h 13% · 7d 44% · sess $12.50");
    }
    #[test]
    fn formats_no_rate_limits() {
        assert_eq!(
            format_statusline(r#"{"cost":{"total_cost_usd":2.0}}"#),
            "bal sess $2.00"
        );
    }
    #[test]
    fn formats_empty_payload() {
        assert_eq!(format_statusline("{}"), "bal (no rate-limit data yet)");
    }
    #[test]
    fn parse_error_is_nonempty_fallback_not_panic() {
        assert_eq!(format_statusline("not json"), "bal (statusline parse error)");
    }
}
```

- [ ] **Step 3: Wire the dispatch arm** in `main()` — add after the `"settings" => cmd_settings(),` line:

```rust
        "settings" => cmd_settings(),
        "statusline" => cmd_statusline(),
```

- [ ] **Step 4: Add to `print_help()`** — after the `settings` line:

```rust
    eprintln!("  balanze-cli settings              Print current settings.json contents");
    eprintln!("  balanze-cli statusline            Read Claude Code's statusLine JSON on stdin,");
    eprintln!("                                print a one-line status (used as Claude Code's");
    eprintln!("                                statusLine command — see `balanze-cli setup`).");
```

- [ ] **Step 5: Verify + commit:**
Run the two Verify commands above; then `cargo build -p balanze_cli && cargo clippy -p balanze_cli --all-targets -- -D warnings && cargo test --workspace && cargo fmt -p balanze_cli -- --check`.
```bash
git add crates/balanze_cli/Cargo.toml crates/balanze_cli/src/main.rs
git commit -m "feat(balanze_cli): add `statusline` subcommand (Claude Code statusLine consumer)"
```

---

### Task 4: `claude_statusline::wiring` — own the `statusLine` stanza in Claude `settings.json`

**Goal:** Locate Claude Code's `settings.json` (dual-path) and idempotently, atomically, reversibly set its `statusLine` to call `balanze-cli statusline`, preserving every other key — mirroring `anthropic_oauth::write_back`'s technique.

**Files:**
- Create: `crates/claude_statusline/src/wiring.rs`
- Modify: `crates/claude_statusline/src/lib.rs` (add `pub mod wiring;` + `pub use` + restore the wiring sentence in the module doc)

**Acceptance Criteria:**
- [ ] `locate_settings_path() -> Result<PathBuf, StatuslineError>` searches, first-existing-wins: `$XDG_CONFIG_HOME/claude/settings.json`, `~/.claude/settings.json`, `~/.config/claude/settings.json` (mirror `anthropic_oauth::credentials::candidate_paths`); `SettingsMissing { searched }` if none exist.
- [ ] `read_wire_status(path) -> Result<WireStatus, StatuslineError>` returns `Unwired` (no `statusLine` key), `WiredToBalanze` (statusLine command contains `balanze-cli statusline`), or `OccupiedBy(String)` (a different statusLine command — return its `command` string), or maps malformed JSON → `SettingsMalformed`.
- [ ] `wire_statusline(path, balanze_invocation: &str) -> Result<(), StatuslineError>` sets `statusLine` to `{ "type": "command", "command": <balanze_invocation> }`, preserving all other keys, via read-generic-`Value` → insert → `to_vec_pretty` → tmp + fsync + rename + cleanup (copy `anthropic_oauth::write_back`'s atomic technique verbatim in shape, minus the token-specific bits). It does NOT overwrite an `OccupiedBy` command (caller decides).
- [ ] If `settings.json` is absent at the chosen path, `wire_statusline` may create it as `{"statusLine":{...}}` ONLY when given an explicit create-path (do not invent a location: caller passes the path from `locate_settings_path`, or a default `~/.claude/settings.json` when none exists — a `default_settings_path()` helper).
- [ ] tempfile tests mirror `anthropic_oauth::credentials` tests: preserves other keys; idempotent (wiring twice = same result, second is a no-op `WiredToBalanze`); `OccupiedBy` detection does not clobber; malformed file → `SettingsMalformed`; missing file via `default_settings_path` creates minimal valid JSON; no `.tmp` leftovers.
- [ ] `cargo test -p claude_statusline` green; clippy clean.

**Verify:** `cargo test -p claude_statusline && cargo clippy -p claude_statusline --all-targets -- -D warnings && cargo fmt -p claude_statusline -- --check`

**Steps:**

- [ ] **Step 1: Write failing tempfile tests** in `wiring.rs` `#[cfg(test)] mod tests` (mirror `crates/anthropic_oauth/src/credentials.rs` tests): `wire_preserves_other_keys`, `wire_is_idempotent`, `read_wire_status_detects_occupied`, `malformed_settings_is_settings_malformed`, `default_path_creates_minimal`, `no_tmp_leftovers`.

- [ ] **Step 2: Implement `crates/claude_statusline/src/wiring.rs`** — `locate_settings_path`/`default_settings_path`/`home_dir`/`candidate_paths` copied in shape from `anthropic_oauth::credentials` (substitute `settings.json` for `.credentials.json`, no `claudeAiOauth`); `WireStatus` enum; `read_wire_status`; `wire_statusline` copying `write_back`'s atomic block (read bytes → `serde_json::Value` → `as_object_mut` (or create root object) → `insert("statusLine", json!({"type":"command","command":invocation}))` → `to_vec_pretty` → unique tmp (pid+nanos+seq) → `create_new` → `write_all` → `sync_all` → rename → cleanup on error). No secrets, so no 0o600 requirement, but keep tmp+fsync+rename + cleanup. (Full code: replicate `write_back` lines 109-234 structure with the statusLine insert instead of the three token inserts, and no `SkippedDiskNewer`/`expiresAt` race logic — settings.json has no token-rotation hazard; a plain atomic replace is correct.)

- [ ] **Step 3: Update `crates/claude_statusline/src/lib.rs`** to the full version from Task 1 Step 7 (add `pub mod wiring;` and `pub use wiring::{locate_settings_path, default_settings_path, read_wire_status, wire_statusline, WireStatus};` and restore the module-doc sentence about owning the settings.json stanza).

- [ ] **Step 4: Tests green + gates + commit:**
Run: `cargo test -p claude_statusline && cargo clippy -p claude_statusline --all-targets -- -D warnings && cargo fmt -p claude_statusline -- --check`
```bash
git add crates/claude_statusline/src/wiring.rs crates/claude_statusline/src/lib.rs
git commit -m "feat(claude_statusline): own the statusLine stanza in Claude settings.json (atomic, idempotent)"
```

---

### Task 5: Extend `balanze-cli setup` with the statusLine wiring step

**Goal:** Add a `[5/5]` interactive step to the setup wizard that detects + offers to wire Claude Code's `statusLine` to `balanze-cli statusline`, asking before writing, never clobbering an existing third-party statusLine.

**Files:**
- Modify: `crates/balanze_cli/src/main.rs` (`cmd_setup` step list + a `setup_statusline()` helper + `print_readiness` line; help text)

**Acceptance Criteria:**
- [ ] `cmd_setup` header lists a 5th step; calls `setup_statusline()` as `[5/5]`.
- [ ] `setup_statusline()` uses `claude_statusline::{locate_settings_path,default_settings_path,read_wire_status,wire_statusline}`: if `WiredToBalanze` → report ✓ already wired, no write; if `Unwired`/no file → print the exact change, prompt `Wire it now? [y/N]` (stdin), only write on explicit `y`; if `OccupiedBy(cmd)` → print the existing command and SKIP (do not clobber; tell the user how to do it manually). All writes go through `wire_statusline` (atomic/idempotent).
- [ ] Prompt reads from stdin like the existing key flow; default is **No** (non-destructive). A non-interactive/empty stdin → treated as No (never auto-write the user's settings.json).
- [ ] `print_help()` `setup` description mentions it now also offers statusLine wiring.
- [ ] `cargo test --workspace` green; manual `balanze-cli setup` smoke (interactive) shows the new step.

**Verify:** `cargo build -p balanze_cli && cargo clippy -p balanze_cli --all-targets -- -D warnings && cargo test --workspace && cargo fmt -p balanze_cli -- --check`; manual: `cargo run -p balanze_cli -- setup` (answer N at the statusline prompt) → shows `[5/5]`, no settings.json written.

**Steps:**

- [ ] **Step 1: Update the `cmd_setup` banner + step count** (main.rs:237-264): change "This wizard:" list to 5 items (add "5. Offers to wire Claude Code's statusLine to `balanze-cli statusline`."), change the `[1/4]`..`[4/4]` labels to `/5`, add before the readiness summary:

```rust
    eprintln!("[4/5] OpenAI admin key");
    let openai = setup_openai_key()?;
    eprintln!();

    eprintln!("[5/5] Claude Code statusLine wiring");
    setup_statusline();
    eprintln!();

    eprintln!("Readiness summary");
    print_readiness(&anthropic, &codex, &openai);
```
(Renumber `[1/5]`/`[2/5]`/`[3/5]` on the existing three steps; keep `print_readiness` signature unchanged — statusline wiring is advisory, not a readiness quadrant.)

- [ ] **Step 2: Add `setup_statusline()`** near the other `setup_*`/`check_*` helpers:

```rust
fn setup_statusline() {
    use claude_statusline::{
        default_settings_path, locate_settings_path, read_wire_status, wire_statusline, WireStatus,
    };
    // The invocation Claude Code will run. Bare `balanze-cli` assumes it is
    // on PATH (true after `cargo install`); documented in setup output.
    let invocation = "balanze-cli statusline";

    let path = match locate_settings_path() {
        Ok(p) => p,
        Err(_) => default_settings_path(),
    };
    match read_wire_status(&path) {
        Ok(WireStatus::WiredToBalanze) => {
            eprintln!("  ✓ Claude Code statusLine already calls balanze-cli ({}).", path.display());
            return;
        }
        Ok(WireStatus::OccupiedBy(cmd)) => {
            eprintln!("  ○ Claude Code statusLine is already set to a different command:");
            eprintln!("      {cmd}");
            eprintln!("    Leaving it untouched. To use Balanze, set statusLine.command to");
            eprintln!("    `{invocation}` in {} yourself.", path.display());
            return;
        }
        Ok(WireStatus::Unwired) => {}
        Err(e) => {
            eprintln!("  ✗ Could not read {} ({e}); skipping statusLine wiring.", path.display());
            return;
        }
    }

    eprintln!("  Balanze can wire Claude Code's statusLine to show live 5h/7d quota.");
    eprintln!("  This will set \"statusLine\" in {} to:", path.display());
    eprintln!("      {{ \"type\": \"command\", \"command\": \"{invocation}\" }}");
    eprintln!("  (other settings preserved; reversible by editing that file).");
    eprint!("  Wire it now? [y/N]: ");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        eprintln!("  ○ No input; skipped (settings.json untouched).");
        return;
    }
    if answer.trim().eq_ignore_ascii_case("y") {
        match wire_statusline(&path, invocation) {
            Ok(()) => eprintln!("  ✓ Wired. Restart Claude Code to see the Balanze status line."),
            Err(e) => eprintln!("  ✗ Failed to write {} ({e}); not wired.", path.display()),
        }
    } else {
        eprintln!("  ○ Skipped (settings.json untouched).");
    }
}
```

- [ ] **Step 3: Update `print_help()`** `setup` lines to mention statusLine wiring (append to the existing setup description):
```rust
    eprintln!(
        "                                input), validates it live, stores it. Also offers to"
    );
    eprintln!("                                wire Claude Code's statusLine. Run this first.");
```
(Replace the existing final setup help line accordingly.)

- [ ] **Step 4: Gates + commit:**
Run: `cargo build -p balanze_cli && cargo clippy -p balanze_cli --all-targets -- -D warnings && cargo test --workspace && cargo fmt -p balanze_cli -- --check`
```bash
git add crates/balanze_cli/src/main.rs
git commit -m "feat(balanze_cli): setup offers to wire Claude Code statusLine (ask-first, no clobber)"
```

---

### Task 6: Docs sync — Repo Map, boundary, README, CHANGELOG, prd/design-doc (AGENTS.md §8)

**Goal:** Record the new crate, the new boundary, the new surfaces; mark Track D delivered. Explicitly state Track D did NOT touch Snapshot/compose/coordinator (that's Track E).

**Files:**
- Modify: `AGENTS.md` (Repo Map: add `claude_statusline`; Architectural Boundaries: new entry; Validation Matrix row if warranted)
- Modify: `README.md` (Layout: add the crate; a line that `balanze-cli setup` wires the statusline)
- Modify: `CHANGELOG.md` (`[Unreleased]` Added)
- Modify: `docs/prd.md` (Phase-2 Track D: mark delivered; note Track E still owns the live integration)
- Modify (external, NOT git-added): `~/.gstack/projects/balanze/oszka-main-design-20260514-153159.md` + `spike-statusline-payload-20260519.md` (mark Track D shipped; note D/E line as built)

**Acceptance Criteria:**
- [ ] `AGENTS.md` Repo Map has a one-line `claude_statusline/` entry; a new Architectural-Boundaries item: "only `claude_statusline` knows the statusLine wire format AND owns the `statusLine` key in Claude `settings.json`; nothing else does" (analogous to #1/#11).
- [ ] `README.md` Layout lists `claude_statusline`; the develop/usage prose notes `balanze-cli statusline` + that `setup` offers wiring.
- [ ] `CHANGELOG.md` `[Unreleased]` `### Added` describes the user-facing change (Balanze can now be Claude Code's statusLine; live 5h/7d quota in the shell; setup wires it).
- [ ] `docs/prd.md` Phase-2 Track D bullet notes "delivered (parse + subcommand + setup-wiring); the live-Snapshot integration is Track E (redefined watcher), not done here."
- [ ] No `Snapshot`/`compose()`/`state_coordinator` change anywhere in the Track D diff (grep-confirm).

**Verify:** `git grep -n "claude_statusline" AGENTS.md README.md` shows the additions; `git diff --stat <task6-base>..HEAD -- crates/snapshot_composer crates/state_coordinator` is EMPTY; `cargo test --workspace` unaffected.

**Steps:**

- [ ] **Step 1:** Add to `AGENTS.md` Repo Map (the `crates/` block) a line: `│   ├── claude_statusline/      parses Claude Code's statusLine stdin payload; owns the statusLine stanza in Claude settings.json (read+atomic write)` and add an Architectural-Boundaries numbered entry mirroring the wording of boundary #1/#11.
- [ ] **Step 2:** `README.md` Layout block — add the `claude_statusline` line; in the CLI/develop section add one sentence: "`balanze-cli statusline` is Claude Code's statusLine command (wired by `balanze-cli setup`) — shows live 5h/7d quota in your shell."
- [ ] **Step 3:** `CHANGELOG.md` under `## [Unreleased]` add/extend `### Added`:
```markdown
- **Claude Code statusLine integration.** `balanze-cli statusline` reads
  Claude Code's statusLine JSON and prints live 5h/7d subscription quota +
  session cost in your shell — zero-auth, no rate limit. `balanze-cli setup`
  offers to wire it for you (ask-first, never clobbers an existing
  statusLine; reversible).
```
- [ ] **Step 4:** `docs/prd.md` Phase-2 Track D bullet — append: "**Delivered** (parse crate + `balanze-cli statusline` + setup-wiring). The live-Snapshot integration (statusline-push feeding the coordinator, OAuth demoted to fallback) is Track E's redefined watcher — not done here."
- [ ] **Step 5:** External design doc + spike artifact — add a one-line "Track D shipped 2026-05-20: parse + subcommand + setup-wiring; D/E line held (no Snapshot/compose change); integration is Track E." (in-place, not git-added).
- [ ] **Step 6:** Verify + commit (repo files only):
Run: `git diff --stat origin/main..HEAD -- crates/snapshot_composer crates/state_coordinator crates/balanze_cli/src/main.rs | grep -E 'snapshot_composer|state_coordinator' && echo "BOUNDARY VIOLATION" || echo "boundary clean"` ; `cargo test --workspace`
```bash
git add AGENTS.md README.md CHANGELOG.md docs/prd.md
git commit -m "docs: record Track D (claude_statusline crate, statusline subcommand, setup wiring)"
```

---

### Task 7: Full workspace validation gate

**Goal:** Prove the whole Track D change is green against AGENTS.md §6 and that the D/E boundary held.

**Files:** none (verification only).

**Acceptance Criteria:**
- [ ] `cargo fmt --all -- --check` clean.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] `cargo test --workspace` all pass (incl. `claude_statusline` unit + `real_payload` + `integration_4quadrant.rs` UNCHANGED — proves no Snapshot/serde drift).
- [ ] `bun run check` clean.
- [ ] `git diff --name-only origin/main..HEAD` contains NO `crates/snapshot_composer/`, `crates/state_coordinator/` changes, and no `Snapshot` struct change (the D/E boundary held).
- [ ] Manual: `printf '%s' '<the Task 3 Verify fixture>' | cargo run -q -p balanze_cli -- statusline` prints one line, exit 0; `echo 'x' | cargo run -q -p balanze_cli -- statusline; echo $?` → fallback line, 0.

**Verify:** the four gate commands each exit 0; the boundary grep is empty; manual statusline runs as described (pasted into close notes).

**Steps:**
- [ ] **Step 1:** `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && bun run check` — all exit 0. If `integration_4quadrant.rs` fails, STOP (unexpected Snapshot/serde drift — Track D must not touch it).
- [ ] **Step 2:** `git diff --name-only origin/main..HEAD` — confirm no `snapshot_composer`/`state_coordinator` paths and no `Snapshot` change. Paste the file list.
- [ ] **Step 3:** Manual statusline smoke (both the happy + the bad-input fallback); paste output. No commit (verification task); a failing gate returns to the owning task (AGENTS.md §7).

---

## Self-Review

**Spec coverage:** (1) schema-owning parser crate → Task 1; (2) real-payload/version confirmation → Task 2 (userGate — needs the maintainer's capture); (3) `balanze-cli statusline` subcommand (minimal output; rich + live-integration explicitly deferred to Track E) → Task 3; (4) setup-wiring (ask-first, atomic, no-clobber, reversible) → Tasks 4 (crate-side, mirrors `write_back`) + 5 (wizard UX); (5) D/E boundary (no Snapshot/compose) → asserted in Tasks 3/6/7; (6) §8 docs → Task 6; validation → Task 7. No spec item unmapped.

**Placeholder scan:** every code step shows full code except Task 4 Step 2, which says "replicate `write_back` lines 109-234 structure" — that is a precise, named, in-repo reference (the implementer reads `crates/anthropic_oauth/src/credentials.rs:109-234` and adapts the documented deltas), not a vague placeholder; the test list + acceptance criteria pin the behavior exactly. Acceptable per the same pattern Track C used for "mirror the existing wiremock test."

**Type/name consistency:** `claude_statusline::parse`, `StatuslineSnapshot { rate_limits, session_cost_micro_usd, claude_code_version }`, `RateLimits { five_hour, seven_day }`, `RateWindow { used_percent: f32, resets_at: DateTime<Utc> }`, `StatuslineError::{InvalidJson, SchemaDrift, SettingsMissing, SettingsIo, SettingsMalformed}`, `WireStatus::{Unwired, WiredToBalanze, OccupiedBy}`, `locate_settings_path`/`default_settings_path`/`read_wire_status`/`wire_statusline`, `format_statusline`/`cmd_statusline` — consistent across Tasks 1–7. `RateWindow` field names mirror `anthropic_oauth::CadenceBar` (`used_percent`/`resets_at`) for Track E uniformity.

**Boundary self-check:** Track D adds a crate + a CLI subcommand + a setup step + the crate's own settings.json ownership. It does NOT modify `Snapshot`, `compose()`, `state_coordinator`, or `snapshot_composer` — Tasks 6/7 actively grep-assert this. The §8 schema gate is therefore not triggered (no Snapshot/UsageEvent change); the new crate + new surfaces are the §8 items, covered by the Task 6 doc-sync.
