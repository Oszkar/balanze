# Codex usage enrichment - design

**Date**: 2026-07-08
**Status**: approved (brainstorming), pending implementation plan
**Scope**: local-only. No new provider API, no new credential, no new poll cadence.

## Problem

The OpenAI/Codex column reads thinner than the Anthropic/Claude column. Claude surfaces multiple rolling windows (5h + 7-day) with utilization %, resets, and pace. Codex surfaces a single number. The Codex Analytics dashboard the user sees shows a 5-hour limit, a weekly limit, per-model (GPT-5.3-Codex-Spark) limits, and a credits balance.

We already read the Codex data locally (`codex_local` parses `~/.codex/sessions/**/rollout-*.jsonl`), but we drop most of it before it reaches any surface.

## Investigation findings (ground truth)

Probed across 447 local session files spanning a Codex CLI update (0.130.0 -> 0.142.5 -> 0.143.0), and a fresh round of GPT-5.3-Codex-Spark + gpt-5.4-mini usage:

- The `token_count` event's `rate_limits` block carries `primary`, `secondary`, `credits`, `individual_limit`, `plan_type`.
- **Two windows exist**, distinguished by `window_minutes`: `300` (5-hour) and `10080` (weekly). Which slot holds which varies:
  - "go" plan: **one** window, in `primary`, `window_minutes: 10080` (weekly). `secondary: null`.
  - "plus"/"pro" plan: **two** windows - `primary` = 5h (300), `secondary` = weekly (10080).
  - Conclusion: **classify by `window_minutes`, never by slot position.** The existing code comments ("primary is 7-day") are stale/inverted for the current plan and were only ever true on "go".
- `individual_limit` (the per-model GPT-5.3-Codex-Spark caps): **null in 0 of 447 files**, including sessions that actually invoked Spark on the updated CLI. Backend/web-dashboard-only. **Not obtainable locally.**
- `credits`: only ever `{has_credits:false, unlimited:false, balance:null|"0"}`. `balance` is a JSON **string**. Effectively "0/none" for this user; `null` on the current "pro" plan.
- `info` carries `total_token_usage` (cumulative session tokens), `last_token_usage` (last turn), `model_context_window` (258400). Present in every `token_count` event; currently discarded by `codex_local`.

## Decisions

1. **Windows (5h + weekly): expose.** Mirror the Claude cell - 5h as headline, weekly as the secondary line.
2. **Per-model Spark caps + credits balance: out of scope.** Backend-only (Spark) or always-zero (credits); would require reading Codex's OAuth token and hitting an undocumented ChatGPT backend endpoint (new secrets surface per AGENTS.md 3.4, new poll per 3.1, brittle scrape). Deferred.
3. **Token/context/burn: parse internally, do NOT expose yet.** Kept as an internal representation on the snapshot (`#[serde(skip)]`), documented, unit-tested, ready to surface when a richer presentation is designed. Exposure and how-to-present are deferred.
4. **Tray: worst-window rule.** The Codex tray figure becomes the worst of {5h, weekly}, consistent with the #157 "title/tooltip name the worst window" design.

## Key architectural consequence: no IPC schema change

`Snapshot.codex_quota: Option<CodexQuotaSnapshot>` embeds the type directly (`state_coordinator/src/snapshot.rs:146`). The frontend **already receives** both `primary` and `secondary` (each with `used_percent`, `window_duration_minutes`, `resets_at`) plus `plan_type` - it just does not render `secondary` today.

Therefore:

- The entire **user-facing** change is presentation logic over already-serialized fields.
- The internal token/context/credits enrichment uses `#[serde(skip)]`, so the serialized `Snapshot` shape is byte-identical.
- **`SNAPSHOT_SCHEMA_VERSION` stays at 3.** No `snapshot.ts` field additions, no `docs/ARCHITECTURE.md` IPC-contract edit, no "ask before schema change" gate triggered.

## Components

### 1. `crates/codex_local` - internal enrichment (parsed, not serialized)

`types.rs`:
- Add a window classifier. Introduce `WindowKind { FiveHour, Weekly, Other }` derived from `window_duration_minutes` (300 -> FiveHour, 10080 -> Weekly, else Other). Not a serialized field; used by accessors.
- Add accessors on `CodexQuotaSnapshot`: `five_hour() -> Option<&RateLimitWindow>` and `weekly() -> Option<&RateLimitWindow>`, each scanning `primary` + `secondary` by kind. These replace positional assumptions everywhere.
- Add `#[serde(skip)] pub tokens: Option<CodexTokenUsage>`:
  - `context_window: u64`
  - `last_input_tokens: u64`, `last_total_tokens: u64` (from `last_token_usage`)
  - `session_total_tokens: u64` (from `total_token_usage.total_tokens`)
  - `recent_burn_tokens_per_min: Option<f64>`
- Add `#[serde(skip)] pub credits: Option<CodexCredits>` (`has_credits: bool`, `balance: Option<i64>` parsed from the string). Internal-only, deferred.
- Fix the inverted doc comments on `RateLimitWindow`/`CodexQuotaSnapshot`.

`parser.rs`:
- Same single-pass scan of the latest session file. Additionally read `payload.info.{total_token_usage,last_token_usage,model_context_window}` from the last `token_count` event.
- Compute `recent_burn_tokens_per_min` from the series of `token_count` events in the file: `(last.session_total_tokens - earlier.session_total_tokens) / minutes_between`. `None` if fewer than 2 events or the delta is negative (context compaction can reset the cumulative counter - guard against it).
- Parse `credits.balance` string -> `i64` (guard non-numeric).
- Missing `info` or any sub-field -> `tokens: None`. The `rate_limits` path and existing `SchemaDrift` behavior are unchanged.

`SCHEMA-NOTES.md`:
- Document the plan-dependent window layout (go = 1 weekly window in `primary`; plus/pro = 5h `primary` + weekly `secondary`) and the classify-by-duration rule.
- Document the **burn limitation**: Codex's cap is percentage-windows, not tokens, so token burn does NOT predict quota exhaustion (unlike Claude, whose cap IS tokens). The eventual actionable token metric is context-window fill. Exposure deferred.
- Document that `individual_limit` (per-model Spark caps) and a real `credits` balance are backend-only and not obtainable from local files.

### 2. Frontend presentation - `src/lib/presentation/quota.ts`

- Add `codexQuota(s: Snapshot)` mirroring `anthropicQuota(s)`. Returns a `{ headline, secondaryPct, plan, tone, staleWindow }`-shaped value:
  - `headline` = the 5h window if present, else the weekly window (go plan). Carries `pct`, `resetsAt`, and the raw window (for `codexElapsedFraction`).
  - `secondaryPct` = the weekly window's `used_percent` when the headline is 5h and a weekly window exists; else `null`.
  - Window selection is by `window_duration_minutes` (300 / 10080), not slot position.
- Reuse existing `codexElapsedFraction(window, fetchedAt)` and `codexWindowExpired(window, fetchedAt)` unchanged (they already take a per-window arg).

### 3. Frontend cell - `src/lib/components/GridView.svelte`

Replace the direct `codex.primary.*` reads (lines 90-94) with the `codexQuota()` result, wired into `QuotaCell` exactly like the Claude cell (lines 52-57):

- `pct` = `headline.pct`
- `used` = `headline.pct`, `elapsed` = `codexElapsedFraction(headline.window, fetched_at) * 100`
- `resetsAt` = `headline.resetsAt`
- `secondary` (the single string slot) = `weekly {secondaryPct}% . {plan}` when a weekly window exists; else `{plan}` (go plan). This mirrors Claude's `7-day X%` secondary string, with the plan appended.
- `stale` = existing `degraded['codex_quota'] || codexWindowExpired(headline.window, fetched_at)`.

No token line in the cell (deferred).

### 4. Tray - `src-tauri/src/tauri_sink.rs`

Extend `TrayView` to split Codex into two windows, mirroring `claude_5h`/`claude_7d`:

- Replace `codex: Option<f32>` with `codex_5h: Option<f32>` and `codex_weekly: Option<f32>`.
- In `from_snapshot`, fold each present Codex window into the right slot by `window_duration_minutes` (300 -> codex_5h, else -> codex_weekly, matching the "every non-5h folds into weekly" safety default used for Claude).
- Add `codex_worst()` = max of the two (mirrors `claude_worst()`).
- `worst()` gains `("Codex 5h", codex_5h)` and `("Codex wk", codex_weekly)` entries (replacing the single `("Codex", codex)`), so the ring/header can name the worst Codex window.
- `has_data()` includes both Codex slots.
- `tray_title`: `Codex Y%` uses `codex_worst()`.
- `tray_tooltip`: the Codex line splits into `5h X%  wk Y%` (mirrors the Claude `5h/7d` line), from the two slots.

### 5. CLI - `crates/balanze_cli/src/render.rs`

- Use the new `five_hour()`/`weekly()` accessors so window labels are correct regardless of slot. The full render already prints primary + optional secondary; relabel them "5h"/"weekly" by kind rather than "primary"/"secondary".
- No token/context display (deferred).

### 6. Statusline - unchanged behavior

`crates/statusline_render` continues to show `<>Codex N%`. `N` should be the worst window (max of 5h/weekly) for consistency with the tray; if the current `CrossProvider.codex_used_percent` producer already sources a single window, switch it to the worst via the same accessors. No new fields.

## Data flow

```
~/.codex/sessions/**/rollout-*.jsonl
  -> codex_local::read_codex_quota()
       -> CodexQuotaSnapshot { primary, secondary, plan_type, rate_limit_reached,
                               #[skip] tokens, #[skip] credits }
  -> snapshot_composer -> Snapshot.codex_quota  (tokens/credits present in-process, absent on the wire)
       -> TauriSink/TrayView  (in-process: could read tokens; currently reads windows only)
       -> IPC Snapshot (serialized: windows only) -> GridView via codexQuota()
       -> balanze-cli render (in-process struct)
```

## Error handling

- Every new field is `Option`; absence never errors. "Codex not installed / no sessions" stays the neutral `Ok(None)` path.
- Non-numeric `credits.balance`, missing `info`, `< 2` burn samples, negative burn delta -> the relevant sub-field is `None`, snapshot still returned.
- Existing `SchemaDrift` semantics for a broken `rate_limits`/`token_count` shape are preserved.

## Testing

`codex_local` parser tests (extend existing):
- Window kind classification with `primary`=5h/`secondary`=weekly (plus/pro order).
- Window kind classification with a single `primary`=weekly window, `secondary: null` (go order) - `five_hour()` is `None`, `weekly()` is `Some`.
- `tokens` populated from `info` (context window, session/last totals).
- `recent_burn_tokens_per_min`: >= 2 monotonic events -> Some; 1 event -> None; non-monotonic (counter reset) -> None.
- `credits.balance` string "0" -> `Some(0)`; `null` -> `None`; non-numeric -> `None`.
- Missing `info` -> `tokens: None`, rate limits still parsed.
- Serialization test: a `CodexQuotaSnapshot` with `tokens`/`credits` populated serializes WITHOUT those keys (guards the no-schema-change invariant).

Frontend:
- `codexQuota()`: two-window (5h headline + weekly secondary) and one-window (go plan: weekly headline, no secondary) cases; classification by duration.
- Visual check in `bun run tauri dev`: Codex cell shows 5h headline + `weekly X% . plan`.

Tray:
- `TrayView::from_snapshot` folds both Codex windows; `worst()` can select a Codex window; `tray_tooltip` shows the `5h/wk` split. Existing tray tests updated for the new slots.

Validation matrix (AGENTS.md 6): `crates/codex_local/**` -> parser tests + documented failure modes + real-data smoke (`cargo run -p codex_local --example ...`). `src-tauri/src/**` -> `bun run tauri dev` smoke. `src/**` -> `bun run check` + visual. Full workspace: `cargo clippy --workspace --all-targets -- -D warnings`, `cargo nextest run --workspace`, `cargo fmt --all -- --check`.

## Out of scope (explicitly)

- GPT-5.3-Codex-Spark per-model caps (`individual_limit`) - backend-only.
- Real credits balance - backend-only / always zero.
- Any ChatGPT backend API call or Codex OAuth token read.
- Surfacing token/context/burn in any UI (parsed internally, deferred).
- Any `SNAPSHOT_SCHEMA_VERSION` bump or IPC contract change.

## Docs to update in lockstep

- `crates/codex_local/SCHEMA-NOTES.md` (window layout, burn limitation, backend-only fields).
- `README.md` / `docs/PRD.md`: only if the Codex column's described behavior (now 5h + weekly) warrants a line. No architecture/IPC change, so `docs/ARCHITECTURE.md` is untouched unless the crate-map note on `codex_local` needs the "reads token/context internally" addendum.
