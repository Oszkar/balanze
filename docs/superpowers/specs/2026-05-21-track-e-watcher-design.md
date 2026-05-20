# Track E — Live watcher + predictor (v0.2 finale)

**Status:** Spec, awaiting plan.
**Authors:** Oszkar + Claude Opus 4.7
**PRD:** `docs/prd.md` Phase 2 Track E
**Architecture:** `AGENTS.md` §4 boundaries 4, 7, 12

## 1. Goal

Make the data layer live. Today `balanze-cli` is single-shot: each invocation
fetches everything fresh. Track E adds a long-running mode that updates the
snapshot in place as the underlying signals change — JSONL writes, Claude
Code statusLine pushes, OAuth poll, OpenAI Costs poll — plus a warm-up-aware
predictor of remaining quota. The Tauri UI is still v0.3; Track E exists to
prove the data spine *works live* before the UI lands on top of it.

Successful Track E means: with `balanze-cli --watch` running, an active
Claude Code session updates the 4-quadrant display within a few hundred
milliseconds of each user turn, without any HTTP fetch.

## 2. Settled architectural decisions

These were brainstormed and locked before this spec was written.

### 2.1 Statusline IPC: file-based snapshot

`balanze-cli statusline` (today: parses stdin, prints status, exits) gains
an atomic write of the parsed payload to
`<data_dir>/balanze/statusline.snapshot.json`. `<data_dir>` is the platform
data dir from the `directories` crate — same resolution as `settings.json`.

The watcher `notify`-watches that file. Push from Claude Code → atomic
file write → notify fires → watcher reads → coordinator merges. No socket,
no shared-memory IPC, no cross-process locking; same atomic write pattern
already used for `settings.json` and `.credentials.json`.

### 2.2 Predictor output: Snapshot field

`Snapshot::prediction: Option<Prediction>`. The predictor is a pure
function the coordinator calls inline after every successful JSONL/OAuth
merge — no new `Source` variant (the predictor has no I/O failure modes),
no new `Sink` method. No `prediction_error` slot: the `Insufficient` state
covers warm-up; a true compute failure is treated as a bug and panics in
debug builds.

### 2.3 Sink shape: unchanged

The existing 2-method `Sink` trait (`on_snapshot(&Snapshot)`,
`on_degraded(Source, &str)`) is already the minimum a future `TauriSink`
will satisfy. No extension.

### 2.4 Channel topology: single mpsc

The existing `StateCoordinatorHandle` is already cloneable and backed by
one bounded mpsc (default capacity 64). Every watcher task and poller
holds a clone and sends `StateMsg::Update` through it. No multi-channel /
priority-routing logic — backpressure is per-handle.

### 2.5 OAuth fallback: always-on poll, render-time dedup

OAuth poll runs unconditionally at the §3.1 5-min floor.
`backoff::standard()` governs retry timing on failure (30s × 2ⁿ, cap
10 min). The snapshot stores both `claude_oauth` and `claude_statusline`
data; the consumer (CLI render today, Tauri UI v0.3) prefers whichever
has the newer `fetched_at`. No inter-source coupling at the producer
side.

## 3. New crates

### 3.1 `predictor`

Pure-function crate. No I/O, no `tokio::spawn`, no logging above `debug`.
Pure functions on data slices (AGENTS.md §4 boundary #2). Public surface:

```rust
pub struct Prediction {
    pub state: PredictionState,
    pub eta_to_cap: Option<Duration>,   // None when state == Insufficient
    pub eta_to_reset: Duration,         // Always present (deterministic from resets_at)
    pub computed_at: DateTime<Utc>,
}

pub enum PredictionState {
    /// First 15 min after window reset, OR fewer than 10 events seen
    /// since reset. The predictor refuses to estimate.
    Insufficient,
    /// Have enough data but EWMA variance is high — show the number with a
    /// "± wide" caveat at render time.
    Uncertain,
    /// EWMA stable. ETA is honestly informative.
    Confident,
}

pub struct WindowSnapshot {
    pub ts: DateTime<Utc>,
    pub used_pct: f64,
}

pub fn predict(
    now: DateTime<Utc>,
    window: &WindowSummary,
    history: &[WindowSnapshot],
    window_reset: DateTime<Utc>,
) -> Prediction;
```

Warm-up gate is explicit (the design doc's "no confidently-wrong numbers
after reset" rule). EWMA smoothing factor and variance threshold are
constants in the crate, justified inline with the choice rationale.

The coordinator owns a 128-entry ring buffer of `WindowSnapshot`. EWMA
itself carries a single accumulator, so the buffer exists only to feed
the variance check that gates `Uncertain` vs `Confident`. 128 entries
gives a comfortable variance signal across a busy session (debounced
notify bursts plus the 60s safety poll fill it in minutes, not hours).
After every `merge_partial` of `ClaudeJsonl` or `ClaudeOAuth`, the
coordinator computes a new prediction and stores it in the snapshot.
Pure computation, no allocation outside the buffer.

### 3.2 `watcher`

Hosts the live loop. Spawns four long-running tokio tasks, each holding
a clone of `StateCoordinatorHandle`.

**Tasks:**

| Task | Trigger | Cadence | Emits |
|---|---|---|---|
| JSONL notify | `notify` on `<claude_home>/projects/**/*.jsonl` | debounce 300ms | `Update(ClaudeJsonl)` + `Update(AnthropicApiCost)` |
| Statusline notify | `notify` on the snapshot file | debounce 100ms | `Update(ClaudeStatusline)` |
| OAuth poll | `tokio::time::interval(5min)` | 5min + `backoff::standard()` on err | `Update(ClaudeOAuth)` |
| OpenAI Costs poll | `tokio::time::interval(5min)` | 5min + `backoff::standard()` on err | `Update(OpenAiCosts)` |
| 60s safety poll | `tokio::time::interval(60s)` | unconditional full rewalk | re-emits all five sources (including Codex, which is otherwise not in the live loop) |

**Codex is not in the live loop.** Codex quota changes slowly enough that
the 60s safety poll's full rewalk catches it; live `notify` watching the
Codex sessions tree adds complexity for a low-value cell. (Single-shot
behavior from v0.1's `codex_local` is preserved.)

**Public API:**

```rust
pub struct Watcher;

impl Watcher {
    pub fn spawn(
        handle: StateCoordinatorHandle,
        settings: &Settings,
    ) -> Vec<JoinHandle<Result<(), WatcherError>>>;
}

#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    #[error("notify watcher exhausted file descriptors; falling back to polling")]
    NotifyExhausted,
    #[error("supervised task panicked: {source:?}: {message}")]
    TaskPanicked { source: Source, message: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
```

**Supervisor:** the caller (`balanze-cli --watch`) holds the `Vec<JoinHandle>`
and runs them under `tokio::select!`. A panic in one task returns
`WatcherError::TaskPanicked`; the supervisor logs at `error!` and respawns
that task after a 5-second backoff. Process-level exit is reserved for
all-tasks-dead.

**Notify exhaustion:** on Linux the kernel `fs.inotify.max_user_watches`
limit can be hit by a deep `~/.claude/projects/` tree. On
`WatcherError::NotifyExhausted`, the JSONL task drops the `notify`
subscription and falls back to a 60s poll loop. Logged at `error!` once
per transition.

### 3.3 `claude_statusline` extensions

The Track D crate gains:

- A `StatuslineFilePayload` type that wraps the parsed snapshot with a
  `schema_version: u8` and `captured_at: DateTime<Utc>` envelope (so
  future-proof: a producer at schema_version 2 can ship before all
  consumers know about it).
- `atomic_write_snapshot(path: &Path, payload: &StatuslineFilePayload) -> Result<()>`
  — tmp + fsync + rename, preserves existing permissions if the file
  already exists. Same pattern as `anthropic_oauth::credentials::write_back`.
- `read_snapshot(path: &Path) -> Result<StatuslineFilePayload>` for the
  watcher.

`claude_statusline` remains the sole owner of the wire format (boundary
#12 unchanged). The file is non-secret — `session_id` is debug-only,
redacted at render unless `-v`.

## 4. Schema changes (§8 — change-control gate)

### 4.1 `state_coordinator::messages`

```rust
pub enum Source {
    ClaudeOAuth, ClaudeJsonl, AnthropicApiCost, CodexQuota, OpenAiCosts,
    ClaudeStatusline,  // NEW
}

pub enum SourcePartial {
    /* existing 5 */
    ClaudeStatusline(claude_statusline::StatuslineFilePayload),  // NEW
}
```

### 4.2 `state_coordinator::snapshot`

```rust
pub struct Snapshot {
    /* existing fields */
    pub claude_statusline: Option<StatuslineFilePayload>,  // NEW
    pub claude_statusline_error: Option<String>,            // NEW
    pub prediction: Option<Prediction>,                     // NEW (no error slot)
}
```

`merge_partial`, `record_error`, and `Snapshot::empty` are extended for
`ClaudeStatusline`. `merge_partial` for the JSONL and OAuth cases gains a
final "recompute prediction" call. The prediction field is set directly
by the coordinator, never by `merge_partial`.

### 4.3 `balanze-cli` JSON DTO

`crates/balanze_cli/src/json_output.rs` gains two top-level cells:

```jsonc
{
  // existing cells
  "claude_statusline": {
    "schema_version": 1,
    "captured_at": "2026-05-21T04:27:42Z",
    "five_hour": { "used_percentage": 82.0, "resets_at": "..." },
    "seven_day": { "used_percentage": 88.0, "resets_at": "..." },
    "session_cost_usd": 3.42,
    "source": "claude_code_statusline",
    "confidence": "estimate"  // session_cost is an estimate per Track C
  },
  "prediction": {
    "state": "Confident",
    "eta_to_cap_seconds": 4823,
    "eta_to_reset_seconds": 9000,
    "computed_at": "2026-05-21T04:27:42Z",
    "source": "predictor_ewma",
    "confidence": "estimate"
  }
}
```

Identifiers (`session_id`) follow the existing `-v` redaction rule. The
schema update is documented in AGENTS.md §2.1 (per the row's own rule:
"Schema changes require updating that module's tests + this row +
`README.md`").

### 4.4 `settings::Settings`

Add one field:

```rust
pub oauth_poll_interval_secs: u32,  // default 300 (5 min — the §3.1 floor)
```

No other settings changes. The 60s safety-poll cadence is a constant
inside `watcher` (not user-tunable; YAGNI).

## 5. `--watch` CLI mode

```
balanze-cli --watch                  human refresh; ANSI clear+redraw on TTY,
                                     append-only on non-TTY
balanze-cli --watch --json           one JSON Snapshot per line; no debounce
balanze-cli status --watch [--json]  same flags; --watch elevates to long-running
```

Two new sinks in `balanze_cli`:

- **`StdoutSink`**: debounces 200ms (collapses rapid notify bursts), reprints
  the 4-quadrant compact view via the existing renderer.
- **`JsonlSink`**: emits one `Snapshot` per line via the existing JSON DTO;
  no debounce (machine consumers want every change).

**Signal handling:** SIGINT / Ctrl+C → drop watcher join handles → drain
coordinator → exit 0. Implemented via `tokio::signal::ctrl_c()` racing
against the supervisor's `select!`.

**Non-TTY behavior:** if `!std::io::stdout().is_terminal()`, `StdoutSink`
disables ANSI clearing and prints each refresh on its own line with a
separator (matches typical `tail -f` UX in pipes).

## 6. Pre-v0.3 Sink-seam checkpoint

This is the #1 remaining roadmap risk per the v0.2 re-plan. Goal: prove
the seam compiles + the shape is right before v0.3 builds Tauri on top.

**Deliverables:**

1. The two real sinks above (`StdoutSink`, `JsonlSink`) exercise the
   `Sink` trait against a live coordinator and a live watcher — that
   alone goes well beyond the v0.1 `NullSink`/`LogSink` coverage.
2. A `TauriSink` skeleton lives in `src-tauri/src/tauri_sink.rs`:

```rust
pub struct TauriSink {
    app: tauri::AppHandle,
    last_painted: Option<(ColorBucket, String)>,  // §3.1 dedup
}

impl Sink for TauriSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        // TODO(v0.3): emit("usage_updated", snapshot)
        // TODO(v0.3): compute (ColorBucket, title); skip set_icon/set_title
        //             if (bucket, title) == self.last_painted
        let _ = (self.app, snapshot, &mut self.last_painted);
    }
    fn on_degraded(&mut self, source: Source, error: &str) {
        // TODO(v0.3): emit("degraded_state", { source, error })
        let _ = (self.app, source, error);
    }
}
```

If this file compiles inside `src-tauri` against the current
`state_coordinator` crate, the seam is correctly shaped. No runtime
wiring; the file is `#[allow(dead_code)]` until v0.3 instantiates it.
The Track E integration test asserts `src-tauri` compiles.

## 7. Criterion benchmarks

Per `docs/prd.md` Phase 2 promoting TODO-002 to Track E. Three benches
land:

- `crates/claude_cost/benches/compute_cost.rs`: 10 000 events through
  `compute_cost`. Budget: < 5 ms.
- `crates/claude_parser/benches/incremental_parser.rs`:
  `IncrementalParser::tick` reading N new lines from a primed cursor.
  Budget: < 200 µs per 100 new lines.
- `crates/window/benches/summarize_window.rs`: 5-hour slice with 10 000
  events. Budget: < 1 ms.

Baselines saved as JSON to `crates/<crate>/benches/baseline.json` and
committed. CI does not run criterion (slow); the maintainer runs them
locally before tagging and refuses to land regressions > 20% without
justification.

## 8. Failure modes

No new `DegradedState` enum; the existing per-source `_error` slot
pattern covers all cases.

| Condition | Behavior |
|---|---|
| Statusline file missing | `claude_statusline_error = Some("file missing — wire `balanze-cli statusline` as your Claude Code statusLine command")`. Not fatal — many users won't have it wired. |
| Statusline file schema drift | `claude_statusline_error = Some("schema drift: <details>")`. Render falls back to OAuth. |
| Statusline file stale (> 30 min) | Not an error. Render-time dedup just prefers OAuth's newer `fetched_at`. |
| Watcher task panics | Supervisor restarts after 5s backoff. Logged at `error!`. |
| Notify exhausts FDs | Drop notify subscription, fall back to 60s polling. Logged at `error!` once. |
| OAuth 401 | Already handled by `anthropic_oauth`'s pre-flight + on-401 refresh. Surfaces as `AuthExpired` if refresh also fails. |
| OpenAI key missing | `openai_error` stays `Some("key not configured")` — current behavior preserved. |

## 9. Changes to `AGENTS.md`

- Repo Map: add `watcher/` and `predictor/` rows.
- §4 boundary #4 ("watcher owns notify + the debounce + the 60s safety
  poll"): concretize — list the four watched paths
  (`<claude_home>/projects/**/*.jsonl`, the statusline snapshot file,
  the two HTTP endpoints behind their respective pollers).
- §2.1: add the `oauth_poll_interval_secs` settings row + the two new
  JSON DTO cells.
- §3.1: confirm the 5-min OAuth cadence as enforced by the always-on
  poller, not as a fallback policy.
- §6 Validation Matrix: add a `watcher/predictor` row requiring
  `cargo bench --no-run` to compile (no regression check in CI).

## 10. Out of scope

- Tauri runtime integration (TauriSink stays a compile-only skeleton).
- Predictor threshold alerting (v0.3).
- Anthropic Console cookie-paste integration (demoted, opt-in).
- Settings UI changes — the new `oauth_poll_interval_secs` lands as a
  settings.json field only; UI exposure is v0.3.
- Cross-device sync of the statusline snapshot file (v1+).

## 11. Sequencing (refined to plan order)

The plan that follows this spec sequences work as:

1. `predictor` crate (pure; small; unblocks coordinator changes).
2. `claude_statusline` extensions (file payload + atomic write/read).
3. `state_coordinator` schema additions (Source/SourcePartial/Snapshot).
4. `balanze-cli statusline` writes the snapshot file (extends existing
   subcommand).
5. `watcher` crate (the bulk — 4 tasks + supervisor + restart logic).
6. `balanze-cli --watch` mode + `StdoutSink` + `JsonlSink` + SIGINT.
7. `TauriSink` skeleton in `src-tauri/`.
8. Criterion benches + baselines.
9. Doc updates (AGENTS.md, PRD Phase 2 "shipped", CHANGELOG Unreleased).

Each step is independently testable. The Track E integration test —
end-to-end watcher feeding a `RecordingSink`, with fixture JSONL writes
and a fixture statusline file write — lands with step 5 and is extended
incrementally through steps 6 and 7.

## 12. Open questions resolved by this spec

None — every decision is settled above. Plan can begin immediately after
user review.
