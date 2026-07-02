# Statusline generic rate windows - Design

Status: approved (design), pre-implementation
Date: 2026-07-02
Context: investigating Claude Sonnet 5 + Claude Fable 5 support surfaced that Anthropic gave Fable its own weekly usage bucket (a 3rd cadence, alongside "Current session" and "All models") on claude.ai's "Plan usage limits" page, live through 2026-07-07 before Fable moves to metered usage-credits billing.

## 1. Goal and framing

The OAuth ingestion path (`anthropic_oauth::ClaudeOAuthSnapshot`) already parses an arbitrary number of named cadence bars generically - it has no allowlist, and unknown keys get a titlecased fallback label. That path was already hardened once before, for exactly this class of problem (the code comments reference `seven_day_sonnet` / `seven_day_opus` from a past real-account payload).

The statusLine ingestion path (`claude_statusline::RateLimits`) was not: it is a fixed `{ five_hour, seven_day }` struct, mirrored 1:1 into the frontend TS types. A new named window in Claude Code's own statusLine JSON (if Anthropic ever adds one there) would silently serde-drop today, with no error and no warning.

This design applies the same hardening already proven on the OAuth side to the statusLine side, using the OAuth side's "generic storage + named accessors" shape rather than reinventing one.

**Not in scope:** whether Claude Code's statusLine JSON actually carries a Fable-specific window today is unconfirmed and not required to justify this change - the gap (silent drop of any unknown key) is real and pre-existing regardless of Fable specifically.

## 2. Resolved decisions

| # | Decision | Choice | Rationale |
|---|---|---|---|
| D1 | Grid view (popover default) | No change | Deliberately terse (headline + one secondary bar) by existing design intent; Cards view is the existing "richer" escape hatch. Confirmed with the user before scoping the rest of this design. |
| D2 | `RateLimits` shape | Generic storage + named accessors (mirrors `ClaudeOAuthSnapshot`) | `RateLimits { windows: Vec<RateWindow> }` plus `.five_hour()` / `.seven_day()` lookup methods. Same generality as a fully-generic rewrite (both are `Vec`-backed), but the 4 non-presentation consumers get a one-line method-call swap instead of restructuring their access pattern. Considered and rejected: (a) full rewrite with no named accessors - same generality, larger diff, no benefit; (c) additive-only (`extra: Vec<RateWindow>` alongside the existing two named fields) - smaller diff but leaves two permanent lookup mechanisms on the type. |
| D3 | Label synthesis for unknown keys | Small private helper local to `claude_statusline`, not shared with `anthropic_oauth::cadence_label()` | The two crates' helpers would be near-duplicates (~15 lines), but introducing a cross-crate dependency (or a new shared-utility crate) for that is premature: the project's own PRD explicitly earmarks "prove the connector abstraction" for if/when a third provider is added, not before (rule-of-three). Revisit only if that happens. |
| D4 | `statusline_render`'s rendered terminal text | No change | The rendered line (`⌛5h 17% 📅7d 2%`) is a shell-prompt segment with real-estate limits; it stays fixed at exactly the two named windows regardless of how many the data model now carries. This is a presentation decision, independent of the data-model hardening. |
| D5 | CLI `--json` schema | Extended additively | `JsonClaudeStatusline` gains a `windows: Vec<JsonRateWindow>` field (mirrors the existing `claude_oauth.cadences` field), and `JsonRateWindow` gains `key` / `label`. `five_hour` / `seven_day` fields stay in place, now carrying the two new sub-fields too. Purely additive - no removal, no rename - so it does not require bumping the top-level `--json` document's `schema_version` (reserved for breaking changes), but does require the CLI-schema doc/test updates the change-control rule calls for. |
| D6 | On-disk `statusline.snapshot.json` `SCHEMA_VERSION` | Bump required | Unlike the CLI JSON, this is a strict hard-checked probe between Balanze's own processes (writer: `balanze-cli statusline`; reader: `watcher::tasks::statusline`), and the shape change is a real breaking change to that wire format. The existing probe-then-parse infrastructure already turns a mismatch into a clean `FileIoError::SchemaDrift` / stale-read, so this exercises an existing path rather than adding new failure handling. |

## 3. Data model

```rust
// crates/claude_statusline/src/types.rs
pub struct RateWindow {
    pub key: String,           // raw wire key: "five_hour" | "seven_day" | any future key
    pub label: String,         // synthesized display label, computed at parse time
    pub used_percent: f32,
    pub resets_at: DateTime<Utc>,
}

pub struct RateLimits {
    pub windows: Vec<RateWindow>,
}

impl RateLimits {
    pub fn five_hour(&self) -> Option<&RateWindow> {
        self.windows.iter().find(|w| w.key == "five_hour")
    }
    pub fn seven_day(&self) -> Option<&RateWindow> {
        self.windows.iter().find(|w| w.key == "seven_day")
    }
}
```

`label` is computed once in Rust at parse time (never re-derived in the frontend), mirroring `anthropic_oauth::client.rs::cadence_label()` / `titlecase_key()`: a curated match for `"five_hour"` -> `"5-hour"` and `"seven_day"` -> `"7-day"`, titlecased fallback for anything else (e.g. `"seven_day_fable"` -> `"Seven Day Fable"`).

The frontend `RateWindow` / `RateLimits` TS types in `src/lib/types/snapshot.ts` mirror this 1:1.

## 4. Parser behavior contract

`claude_statusline::parse.rs` has real, tested drop/degrade/error semantics per window today (distinct from, and stricter than, `anthropic_oauth`'s looser "malformed cadence -> silently skip" behavior). Generalizing to an arbitrary key set must preserve all of it, not adopt the OAuth side's looser rules:

| Input for a given key | Result |
|---|---|
| `rate_limits` object absent entirely | `RateLimits` is `None` (unchanged) |
| A key absent from the object | No window for that key (silently) |
| A key present with value `null` | Treated as absent ("block-level null = absent", unchanged) |
| A key present, window object missing `used_percentage` or `resets_at` | That window is dropped, `warn!` logged, rest of the payload survives (unchanged) |
| A key present, `used_percentage` or `resets_at` is explicitly `null` | Whole payload fails as `SchemaDrift` (unchanged - corruption, not absence) |
| A key present, `resets_at` out of chrono's representable range | That window is dropped, `warn!` logged, rest survives (unchanged) |
| A key present, well-formed, not `"five_hour"` / `"seven_day"` | **New:** parses into a `RateWindow` with a titlecased fallback `label`, included in `windows` |

Only the last row is new behavior; everything else is today's contract applied per-key instead of to two hardcoded field names.

**Ordering:** `windows` is sorted the same way `anthropic_oauth::client.rs::cadence_sort_key` already sorts `cadences` - `five_hour` first, `seven_day` second, any other key after (ties broken alphabetically by key). Mirroring the existing precedent instead of leaving it to incidental JSON-object iteration order.

## 5. Consumer changes

Three consumers are mechanical (field access -> method call; output unchanged):

- **`statusline_render::render.rs`** - `rl.five_hour` -> `rl.five_hour()`, `rl.seven_day` -> `rl.seven_day()`. Rendered terminal text is byte-for-byte identical to today (D4).
- **`src-tauri/src/tauri_sink.rs`** - same swap, for the tray "has quota" check.
- **`crates/balanze_cli/src/json_output.rs`** - same swap for the existing `five_hour` / `seven_day` fields, plus the new `windows` field (D5):
  ```rust
  #[derive(Serialize)]
  struct JsonRateWindow {
      key: String,
      label: String,
      used_percent: f32,
      resets_at: DateTime<Utc>,
  }

  struct JsonClaudeStatusline {
      schema_version: u8,
      captured_at: DateTime<Utc>,
      five_hour: Option<JsonRateWindow>,
      seven_day: Option<JsonRateWindow>,
      windows: Vec<JsonRateWindow>,   // new
      session_cost_usd: Option<f64>,
      claude_code_version: Option<String>,
      source: &'static str,
      confidence: &'static str,
  }
  ```

One real behavioral change:

- **`src/lib/components/CardsView.svelte`'s statusline branch** goes from hardcoding two pushes (`5-hour`, `7-day`) to mapping every entry in `rate_limits.windows`, mirroring how its OAuth branch already maps every `cadences` entry:
  ```js
  const rl = snapshot.claude_statusline?.payload.rate_limits;
  if (rl?.windows?.length) {
    return rl.windows.map((w) => ({
      label: w.label, used: w.used_percent, elapsed: paceElapsed(w.key),
      tone: quotaTone(w.used_percent), resetsAt: w.resets_at,
      stale: anthStale, title: PROV.anthropicQuotaStatusline.title,
    }));
  }
  ```

**Known nuance, not a new problem:** `elapsed` (the pace tick) is looked up from `snapshot.pace`, which `snapshot_composer::compose` derives only from OAuth cadences (`pace_for_oauth`), never from statusline windows directly. A statusline-only window with no matching OAuth cadence key renders with no pace tick (`elapsed: null`) - already true for `five_hour` / `seven_day` today whenever OAuth isn't configured; unaffected by this change.

## 6. Schema & docs updates

- Bump `claude_statusline`'s on-disk `statusline.snapshot.json` `SCHEMA_VERSION` (D6).
- Update `docs/ARCHITECTURE.md`: the "on-disk IPC files" table's `statusline.snapshot.json` row, and the CLI `--json` schema paragraph (new `windows` field).
- Update `README.md` wherever the CLI `--json` output shape is documented.
- Update `crates/balanze_cli/src/json_output.rs` tests for the new `windows` field.

## 7. Testing strategy

- **`claude_statusline::parse.rs`** - the existing 11-test suite (drop/degrade/schema-drift semantics) is updated to go through `.five_hour()` / `.seven_day()` instead of field access, asserting identical behavior. New tests: an unknown key produces a titlecased-fallback `RateWindow` in `windows`; a 3-window payload (`five_hour` + `seven_day` + one unknown) yields all three; the explicit-null-is-`SchemaDrift` case is re-verified for a non-named key too, confirming the strict semantics generalized rather than only ever applying to the two originally-hardcoded fields. Per AGENTS.md's test discipline, these new tests are written before the parser implementation changes (load-bearing pure function).
- **`RateLimits::five_hour()` / `seven_day()`** - direct unit tests mirroring `anthropic_oauth::types.rs`'s existing `five_hour_reset()` present/absent tests.
- **`statusline_render`, `tauri_sink.rs`, `json_output.rs`** - existing test fixtures constructing `RateLimits { five_hour: ..., seven_day: ... }` struct literals become `RateLimits { windows: vec![...] }` constructions (mechanical). `json_output.rs` gets one new test asserting the `windows` field carries a 3rd/unknown window through.
- **Real-payload fixture** (`crates/claude_statusline/tests/real_payload.rs`) - stays green unmodified; it only exercises `five_hour` / `seven_day`, now via the accessors.
- **`CardsView`** - extend whatever fixture/gallery mechanism already covers its OAuth branch's multi-cadence rendering to give the statusline branch equivalent coverage with 3 windows.

## 8. Non-goals / boundaries kept

- Grid view (`GridView.svelte` / `quota.ts::anthropicQuota()`) is untouched (D1).
- `statusline_render`'s rendered terminal text stays fixed at exactly two segments (D4).
- No shared cross-provider "named usage window" abstraction between `anthropic_oauth` and `claude_statusline` (D3).
- No change to `claude_cost`'s price table or the OpenAI cost-fetch window bug identified in the same investigation - both are unrelated, tracked separately.

## 9. Risks

- The parser behavior-contract generalization (section 4) is the highest-risk piece: it must preserve five distinct existing semantics (absent / null-as-absent / drop-with-warn / hard-schema-drift / out-of-range-drop) across an arbitrary key set rather than two hardcoded fields. Mitigated by writing the contract table above before implementation and test-first per AGENTS.md.
- Unconfirmed whether Claude Code's statusLine JSON will ever actually carry a window beyond `five_hour` / `seven_day` - this change is justified by closing a real silent-drop gap (parity with the already-hardened OAuth path), not by confirmed evidence Fable (or anything else) appears there today.

## 10. Deferred / explicitly out of scope

- Extending Grid view to show additional cadences (D1) - revisit only if the "nothing" decision stops being sufficient in practice.
- A shared cross-provider window/cadence abstraction (D3) - revisit if/when a third provider is added, per the PRD's own "prove the connector abstraction" framing.
- Any further CLI-side filtering/sorting of `windows` beyond the ordering fixed in section 4 - out of scope; the CLI serializes `windows` as-is.
