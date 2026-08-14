# OpenAI Costs Machine-Wide Gate Implementation Plan

> **For agentic workers:** Execute this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Keep every task green before starting the next one.

**Goal:** Close GitHub issue #204 by enforcing one OpenAI Admin Costs request per 300 seconds for each resolved request identity across the watcher, statusline self-compose, one-shot status, export, setup, doctor, and Tauri key validation.

**Architecture:** Move the existing statusline-only cache and refresh lease into `openai_client`, the sole owner of `/v1/organization/costs`. Replace the single cache entry with a versioned, bounded multi-entry store protected by one store-wide interprocess lease. Before any HTTP request, a caller acquires that short-lived lease, rechecks the store, and durably publishes a tokenized attempt reservation. The lease is then released before network I/O. Success or failure completes that reservation only when its token is still current, so a late request cannot overwrite a successor. Every public month-to-date Costs call goes through this gate and uses one fail-fast HTTP attempt. The statusline remains demand-gated and can render a cached headline as stale; other callers require a full current-month result or surface a typed degraded outcome.

**Tech Stack:** Rust 2024, Tokio, Serde, `atomic_file`, `directories`, `reqwest`, `wiremock`, `tempfile`, `cargo nextest`.

**Source decisions:** GitHub issue #204, the provenance finding resolved in PR #141, and the August 2026 triage decisions. OpenAI organization selection is not supported. The resolved API key, normalized API base URL, and fixed month-to-date query contract form the request identity.

## Prerequisites and Remaining Unknowns

- No blocking product or architecture decisions remain. The literal all-path 300-second invariant, multi-entry schema, conservative fail-closed defaults, and lack of OpenAI organization selection are settled.
- No new crate is required by the current design. `OpenAiCosts` already supports Serde, and the existing workspace provides the atomic writer, cache-directory resolver, and dependency-free lease/token mechanism. A new dependency remains authorized only if implementation proves one is necessary.
- Exact Rust type and method names are implementation details. They may change without reopening the design as long as the invariants and public ownership boundary remain intact.
- Cross-process behavior on Windows and macOS is not accepted on unit tests alone. The existing process-level lease tests, full cross-platform CI workflow, and manual real-endpoint smoke remain release prerequisites.
- The UTC month boundary does not override an active reservation. A request just before the boundary can defer current-month status, export, watcher, setup, doctor, and Tauri validation for the remaining portion of its 300-second window. This user-visible gap is deliberately accepted to preserve one literal invariant on every path.
- Mixed-version coordination and large forward wall-clock jumps remain documented limitations, not blockers. The plan provides best-effort rolling compatibility and conservative rollback handling.

## Global Constraints

- **Authorized operational-contract changes.** This implementation deliberately supersedes two current `AGENTS.md` section 3.1 rules: OpenAI watcher requests no longer use `BackoffPolicy::standard()`, and the 300-second cache/lease is no longer owned only by statusline self-compose. These are approved behavior changes, not interpretations of the old wording. The implementation PR must replace both rules and document why durable all-path reservation makes one fail-fast request safer and simpler.
- **Literal 300-second gate.** One logical Costs fetch means one HTTP request. Do not use `BackoffPolicy::standard()` or honor a short `Retry-After` with an in-call retry. A failed attempt reserves the identity for the full 300 seconds; the next poll interval performs the next attempt.
- **Resolve once.** Each caller resolves or receives one owned OpenAI key. The same exact value drives the request fingerprint and Authorization header. Never re-read the key between reservation and HTTP.
- **Fail closed before HTTP.** If the gate directory, lease, store read, reservation serialization, or durable reservation publication fails, skip the request. Degrade the caller without breaking its process.
- **Best-effort completion.** Once a reservation is durable, a success or failure completion write may fail without breaking the caller. The still-persisted in-flight reservation continues to suppress requests until its 300-second window expires.
- **No cross-key attribution.** Values are reusable only for the exact request fingerprint. A different key or API base URL receives its own store entry and never sees another identity's spend.
- **No raw request identity on disk.** Persist non-secret fingerprints only. Never persist or log the key. Do not persist the raw API base URL in the gate store.
- **One source of truth per success.** A fresh success stores only the full `OpenAiCosts`; its headline is derived from `OpenAiCosts::total_micro_usd`. A migrated legacy entry stores only its bare `total_micro_usd` and `fetched_at`. Use distinct enum variants so one entry can never persist both forms. The legacy-compatible root projection is derived output for rolling compatibility, not another authoritative value.
- **One store-wide lease.** One interprocess lease protects the entire eight-entry JSON store, not one lease per request identity. It serializes only read-migrate-reserve and read-token-check-complete transactions. It is always released before HTTP, so unrelated identities may perform network requests concurrently after their short store transactions.
- **Bounded store.** Retain at most eight request identities. Prune only entries whose latest attempt is outside the 300-second gate, oldest first. Never evict an active entry. If all eight entries are active, fail closed for a ninth identity.
- **UTC month boundary.** A cached success is current only when its `start_time` equals the current UTC month start. A prior-month value may be exposed to the statusline as stale, but must not satisfy status, export, watcher, setup, doctor, or Tauri validation as a current result. Do not special-case month rollover: an attempt immediately before the boundary remains active for its complete 300-second window, so non-statusline callers deliberately receive a deferred/degraded result until it expires.
- **Clock rollback.** A future attempt timestamp is still gated. Under the lease, normalize it once to the current wall clock and persist that correction, then require 300 seconds from the normalized time. A large forward system-clock jump remains an explicit limitation of a cross-process wall-clock protocol.
- **Cache-root scope.** The guarantee covers cooperative Balanze processes for one OS user and one resolved cache root. Processes using different `BALANZE_CACHE_DIR_OVERRIDE` values are separate namespaces.
- **Rolling upgrades.** Read and migrate the legacy single-entry JSON. Write a legacy-compatible root projection for the most recently touched identity so an older statusline process with that same key can observe the cooldown. Do not promise strict enforcement across indefinitely running older binaries or alternating older binaries with different keys.
- **No canonical schema changes.** Do not change `UsageEvent`, `Snapshot`, `SnapshotFilePayload`, CLI `--json`, frontend IPC, `Settings`, `StateMsg`, or the coordinator actor boundary.
- **No new crate.** The fixed workspace crate set stays unchanged. Existing workspace dependencies may move from `statusline_render` to `openai_client`.
- **No em dashes, en dashes, or Unicode ellipsis.** Use `-` and `...` everywhere (AGENTS.md section 3.5).
- **Conventional Commits.** Do not use `--no-verify` or `--no-gpg-sign`.
- **Rust validation after every task:**

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo nextest run --workspace
  ```

## Invariants

The implementation is complete only when all of these are true:

1. A durable reservation exists before any supported production path sends `/v1/organization/costs`.
2. The latest attempt start, not its completion time, opens the 300-second gate.
3. Success, 401, 403, 429, 5xx, transport failure, response-shape failure, timeout, and process death all suppress the next request for the same identity.
4. Exactly 300 seconds after the latest attempt start, a new reservation may be created.
5. A completion mutates the store only when its token matches the current attempt token.
6. A cached full success from the current UTC month satisfies every caller inside the gate without HTTP.
7. A cached legacy headline or prior-month success never masquerades as a current full result.
8. Different request fingerprints retain independent entries and values.
9. Store capacity pressure never evicts an entry still inside its 300-second gate.
10. Gate storage failure cannot produce an ungated request.
11. No production crate outside `openai_client` can call the raw Costs HTTP function.
12. A statusline configuration without `{openai_cost}` does not resolve the key, read the gate, acquire a lease, or issue HTTP.
13. One store-wide lease serializes every mutation of the shared multi-entry document, while HTTP remains outside the lease.

## File Structure

**Create:**

| Path | Responsibility |
|---|---|
| `crates/openai_client/src/gate.rs` | Request fingerprints, versioned multi-entry store, legacy migration/projection, one store-wide interprocess lease, reservation and completion protocol |

**Delete after migration:**

| Path | Reason |
|---|---|
| `crates/statusline_render/src/cache.rs` | The gate belongs to the only Costs HTTP client, not to one presentation surface |

**Modify:**

| Path | Change |
|---|---|
| `crates/openai_client/Cargo.toml` | Add existing workspace crates needed by the gate: `atomic_file`, `directories`, and production Tokio time support; add `tempfile` for tests |
| `crates/openai_client/src/lib.rs` | Export the gated month-to-date API and typed gate outcome; stop exporting raw Costs fetch functions |
| `crates/openai_client/src/client.rs` | Keep raw HTTP and parsing private; perform one fail-fast request per reservation |
| `crates/openai_client/src/types.rs` | Add safe persisted failure classification and typed gated error/outcome accessors |
| `crates/openai_client/tests/wiremock_tests.rs` | Drive the public gated API through isolated cache roots and assert request counts |
| `crates/statusline_render/Cargo.toml` | Remove cache-only dependencies no longer used by this crate; do not add a provider-client dependency |
| `crates/statusline_render/src/lib.rs` | Remove the public `cache` module; export the simplified cross-source cell type |
| `crates/statusline_render/src/self_compose.rs` | Remove cache ownership and map a source-provided current/stale OpenAI cell into `CrossProvider` |
| `crates/balanze_cli/src/sources.rs` | Route status, export, and statusline through the gated API; remove caller-owned fingerprinting |
| `crates/balanze_cli/src/statusline.rs` | Stop resolving cache paths and fingerprints; preserve the `{openai_cost}` demand gate |
| `crates/balanze_cli/src/setup.rs` | Delegate key validation to the shared watcher validation adapter instead of calling Costs directly |
| `crates/balanze_cli/tests/integration_statusline_self_compose.rs` | Preserve demand-gate coverage and assert the provider-wide store suppresses repeated subprocess calls |
| `crates/watcher/src/tasks/openai_poll.rs` | Use the provider gate and one fail-fast request per tick |
| `crates/watcher/src/validate.rs` | Use the provider gate and map cached safe failure classifications into `KeyProbe` |
| `AGENTS.md` | Replace the statusline-only and OpenAI retry wording with the all-path gate contract |
| `docs/ARCHITECTURE.md` | Move gate ownership to `openai_client` and document the reservation state machine |
| `docs/TROUBLESHOOTING.md` | Describe the shared gate, cache-root scope, stale statusline behavior, and validation cooldown |
| `docs/src/cli.md` | Note that status and export may reuse the shared current-month Costs result |
| `docs/src/faq.md` | Explain the bounded validation cooldown after any Costs attempt |
| `docs/PRD.md` | Mark provenance-safe statusline cache seeding as the provider-wide gate delivered for the support milestone |
| `CHANGELOG.md` | Record the fixed duplicate-poll and cross-key attribution risks |

**Deliberately unchanged:** `crates/state_coordinator/**`, `crates/snapshot_composer/**`, `src/**`, `Snapshot`, `SnapshotFilePayload`, settings schemas, IPC schemas, and Tauri capabilities.

## Gate State Model

Each request identity owns one entry:

```text
No entry
  |
  | reserve(token, started_at) - durable before HTTP
  v
InFlight(token, started_at) -----------------------------+
  |                                                       |
  | matching completion                                  | process dies or completion write fails
  |                                                       |
  +-> Success(token, started_at) + full last_success      |
  |                                                       |
  +-> Failure(token, started_at, safe failure kind)       |
                                                          |
All three states suppress another request until           |
started_at + 300 seconds. After expiry, a new token <------+
supersedes the old attempt. A completion carrying the old
token is ignored.
```

Use one attempt record rather than unrelated success and failure timestamps:

```rust
struct AttemptRecord {
    token: String,
    started_at: DateTime<Utc>,
    state: AttemptState,
}

enum AttemptState {
    InFlight,
    Success,
    Failure(StoredFailureKind),
}

struct GateEntry {
    key_fingerprint: String,
    last_attempt: AttemptRecord,
    last_success: Option<StoredSuccess>,
}

enum StoredSuccess {
    Full(OpenAiCosts),
    LegacyHeadline {
        total_micro_usd: i64,
        fetched_at: DateTime<Utc>,
    },
}
```

Derive `total_micro_usd` and `fetched_at` from the `Full(OpenAiCosts)` payload through accessors. Never persist a separate headline beside a full result. The legacy root projection may repeat the derived number for compatibility with older binaries, but it is regenerated from the selected entry and is never read as the schema-2 entry's source of truth.

The exact names may change during implementation, but these variants and semantics are load-bearing.

## Task 1: Move the cache primitive and add the versioned multi-entry store

This task creates the provider-owned persistence layer without changing production callers. The existing statusline cache remains temporarily so the tree stays green.

**Files:**

- Create: `crates/openai_client/src/gate.rs`
- Modify: `crates/openai_client/Cargo.toml`
- Modify: `crates/openai_client/src/lib.rs`
- Modify: `crates/openai_client/src/types.rs`

**Interfaces produced:**

- `COSTS_GATE_SECS: i64 = 300`
- `MAX_GATE_IDENTITIES: usize = 8`
- A stable request-fingerprint helper over the exact key, normalized base URL, and a fixed operation tag such as `organization-costs-this-month-v1`
- A path resolver that keeps the existing `<cache>/statusline/openai-cost.json` location and honors `BALANZE_CACHE_DIR_OVERRIDE`
- Internal read, migrate, prune, and durable atomic write operations
- A safe serializable `StoredFailureKind` with no body or secret fields

- [ ] **Step 1: Add failing schema and identity tests**

Add unit tests in `gate.rs` for:

- stable identity for the same key and normalized base URL;
- different keys produce different identities;
- different normalized base URLs produce different identities;
- trailing slashes do not create a second identity;
- neither the raw key nor raw base URL appears in serialized JSON;
- two entries survive a round trip without cross-value reuse;
- the full `OpenAiCosts`, including line items, survives a round trip;
- a fresh full entry serializes one authoritative `total_micro_usd` inside `OpenAiCosts`, with no sibling headline field in the schema-2 entry;
- a legacy `OpenAiCostEntry` JSON document migrates into one entry;
- a migrated legacy entry uses the headline-only variant and carries no invented full result;
- serialization emits the legacy root projection fields `fingerprint`, `total_micro_usd`, `fetched_at`, and `last_failure_at` for the most recently touched key;
- the legacy root projection for a full entry derives its headline from `OpenAiCosts` rather than copying a separately stored value;
- unknown future store schema versions fail closed rather than being treated as an empty cache.

- [ ] **Step 2: Run the focused tests and verify failure**

  ```bash
  cargo nextest run -p openai_client gate
  ```

  Expected: compilation fails because the gate module and schema do not exist.

- [ ] **Step 3: Add dependencies and implement the store**

Move only the persistence and lease-independent helpers at this stage. Use `atomic_file::atomic_write` for publication. Preserve path-only error reporting. Do not swallow store errors inside this module: the public gate API in Task 2 must distinguish a safe fail-closed gate error from a provider error.

The on-disk document is schema version 2 and contains a map keyed by request fingerprint. Model successful data as mutually exclusive `Full(OpenAiCosts)` and `LegacyHeadline { total_micro_usd, fetched_at }` variants. Keep a legacy root projection so the currently shipped statusline reader can parse the file while upgrading. The projection selects the most recently touched entry, derives its headline through the variant accessor, and uses that entry's key-only fingerprint. While an attempt is in flight, project its `started_at` into `last_failure_at` so an older same-key statusline process observes a cooldown instead of fetching.

- [ ] **Step 4: Implement bounded pruning tests first**

Cover:

- nine expired entries prune the oldest to eight;
- an active entry is never pruned in favor of an expired entry;
- eight active entries reject a ninth identity;
- different identities retain independent success and failure state;
- exactly 300 seconds makes an entry eligible for replacement;
- a negative age is active, not expired;
- a future timestamp is normalized once under a mutable store operation and then expires 300 seconds later.

- [ ] **Step 5: Implement pruning and clock normalization**

Prune only while holding the interprocess lease added in Task 2. Keep the store helper pure by accepting `now`. Do not read the clock from several helper layers.

- [ ] **Step 6: Run the task gate**

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo nextest run -p openai_client
  cargo check --workspace
  cargo tree --workspace --duplicates
  ```

- [ ] **Step 7: Commit**

  ```bash
  git add crates/openai_client/Cargo.toml crates/openai_client/src/gate.rs crates/openai_client/src/lib.rs crates/openai_client/src/types.rs Cargo.lock
  git commit -m "refactor(openai): add the provider-owned costs gate store"
  ```

## Task 2: Add durable reservation and the sole public month-to-date Costs API

This task turns the store into the enforcement point. Raw HTTP remains inside `openai_client`; production consumers receive a typed gated result.

**Files:**

- Modify: `crates/openai_client/src/gate.rs`
- Modify: `crates/openai_client/src/client.rs`
- Modify: `crates/openai_client/src/lib.rs`
- Modify: `crates/openai_client/src/types.rs`
- Modify: `crates/openai_client/tests/wiremock_tests.rs`

**Public behavior produced:**

- One public month-to-date function builds the HTTP client from an injected timeout, resolves the shared cache path, reserves the request, performs at most one GET, and completes the reservation.
- Success returns a full current-month `OpenAiCosts`, whether fetched now or reused from the store.
- A typed gate/provider error exposes safe methods such as `cached_total_micro_usd()`, `cached_full_costs()`, `failure_kind()`, `is_retryable()`, and `admin_key_hint()` without exposing a stored response body.
- The existing arbitrary-window raw fetch becomes private to the crate. Move integration coverage that requires it behind the public gated API or into `client.rs` unit tests.

- [ ] **Step 1: Write failing reservation transition tests**

Add deterministic tests with fixed `now` values:

- empty store reserves and returns a token;
- same identity inside 300 seconds is suppressed after success;
- same identity is suppressed after every safe failure kind;
- an in-flight reservation suppresses a contender;
- exactly 300 seconds permits a successor token;
- different identities reserve independently;
- matching-token success stores the full result;
- matching-token failure preserves the prior success and stores only the safe failure kind;
- late success after a successor reservation is ignored;
- late failure after a successor reservation is ignored;
- an attempt one second before a UTC month boundary still blocks a new-month reservation for the remaining 299 seconds;
- completion publication failure leaves the reservation in force;
- reservation publication failure returns an error before the fetch callback can run.

- [ ] **Step 2: Move the existing lease protocol**

Move the unique `create_new` candidate lease, legacy lease recognition, 10-second abandoned-candidate rule, token-checked drop cleanup, and cross-process exclusivity tests from `statusline_render/src/cache.rs` into `openai_client/src/gate.rs`.

Use exactly one lease namespace for the whole gate document. Do not create a lease per request fingerprint. Every reservation or completion rewrites one shared JSON document, so per-identity leases would still need a store-wide merge lock to avoid lost updates. The single store-wide lease is the simpler correct design because the network request is outside the critical section.

The lease now protects short store transactions only:

1. acquire;
2. read and migrate;
3. recheck latest attempt;
4. durably write the reservation;
5. release;
6. perform HTTP;
7. reacquire;
8. token-check and complete best-effort.

Do not hold the lease across HTTP. The durable reservation, not lease lifetime, enforces the 300-second window.

- [ ] **Step 3: Preserve bounded contention behavior**

A contender may poll for committed state or lease availability for at most 250ms. After that it fails closed. Keep the existing 20ms poll cadence. A statusline with stale data returns that value immediately rather than waiting.

Test:

- two threads racing an empty store produce one reservation;
- two different identities serialize their store transactions but may overlap their HTTP work after both reservations are durable;
- two OS processes recovering around an abandoned lease produce at most one reservation;
- a contender observes the winner's durable reservation;
- a live legacy lease blocks the new protocol;
- an unwritable directory results in zero fetch-callback invocations;
- concurrent publications expose only complete JSON documents.

- [ ] **Step 4: Write the public API wiremock tests before implementation**

Adapt `wiremock_tests.rs` so every test uses a unique temporary gate root and the public gated month-to-date function. Add request-count tests for:

- two sequential successful calls -> one GET and the same full result;
- concurrent successful calls -> one GET;
- 401 followed by a second call inside 300 seconds -> one GET total, with the second error classified `AuthInvalid` from stored safe state;
- 403 -> one GET total and preserved admin-key guidance;
- 429 -> one GET total, even when `Retry-After` is shorter than 300 seconds;
- 500 -> one GET total;
- transport timeout -> no in-call retry;
- malformed 200 response -> one GET total;
- different key -> independent GET and independent cached result;
- different API base URL -> independent GET;
- gate write failure -> zero GETs;
- attempt just before UTC month rollover followed by a new-month call inside the active gate -> no second GET and no current full result until the original 300 seconds expire;
- full current-month cached success -> status/export-capable full result with line items.

Delete or invert `costs_retry_on_429_then_succeed`. The required assertion is now exactly one request and no retry.

- [ ] **Step 5: Implement the public API**

Keep request parsing and redaction in `client.rs`. The gate wrapper owns orchestration. Call the raw HTTP function with `BackoffPolicy::fail_fast()` internally or remove the policy parameter from the raw function entirely. No production caller chooses retry policy after this task.

Classify failures before persistence:

| Runtime error | Persisted kind | Retryable to callers? |
|---|---|---|
| HTTP 401 | `AuthInvalid` | No |
| HTTP 403 | `InsufficientScope` | No |
| HTTP 429 | `RateLimited` | Yes |
| HTTP 5xx/other status | `UnexpectedStatus(status)` | Yes |
| transport/timeout | `Network` | Yes |
| parse/shape | `ResponseShape` | Yes |

Persist no response body. The initiating caller may still receive the existing redacted runtime `OpenAiError`; later callers receive the safe stored classification.

- [ ] **Step 6: Make bypasses compile-time impossible**

Stop exporting `fetch_costs`, ungated `costs_this_month`, and ungated `costs_this_month_with` from `openai_client::lib`. Keep the raw function private. Update crate docs to state that the public API is month-to-date and gated.

Verify:

  ```bash
  rg -n "fetch_costs|costs_this_month_with|costs_this_month" crates src-tauri --glob "*.rs"
  ```

Expected after later caller-migration tasks: production uses outside `openai_client` disappear. At this intermediate step the workspace may still name old exports, so do not remove them until a temporary compatibility wrapper or the next task keeps the tree compiling. The final state must expose no ungated production API.

- [ ] **Step 7: Run the task gate**

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo nextest run -p openai_client
  cargo nextest run --workspace
  ```

- [ ] **Step 8: Commit**

  ```bash
  git add crates/openai_client/src/gate.rs crates/openai_client/src/client.rs crates/openai_client/src/lib.rs crates/openai_client/src/types.rs crates/openai_client/tests/wiremock_tests.rs
  git commit -m "fix(openai): reserve costs polls before network access"
  ```

## Task 3: Move statusline self-compose onto the provider gate

The statusline stops owning cache policy. It asks its real source adapter for a current or stale OpenAI cell; `openai_client` decides whether to fetch, reuse, or suppress.

**Files:**

- Delete: `crates/statusline_render/src/cache.rs`
- Modify: `crates/statusline_render/Cargo.toml`
- Modify: `crates/statusline_render/src/lib.rs`
- Modify: `crates/statusline_render/src/self_compose.rs`
- Modify: `crates/balanze_cli/src/sources.rs`
- Modify: `crates/balanze_cli/src/statusline.rs`
- Modify: `crates/balanze_cli/tests/integration_statusline_self_compose.rs`

**Interfaces produced:**

- `statusline_render::OpenAiCell { total_micro_usd: Option<i64>, stale: bool }`
- `CrossSources` supplies an OpenAI cell rather than a raw fetch result.
- `self_compose` no longer accepts cache directory or fingerprint arguments. It remains demand-gated by `want_openai`.
- `LiveCrossSources` resolves the key once only when `{openai_cost}` is requested, then calls the provider gate with its 3-second timeout.

- [ ] **Step 1: Write the simplified renderer tests**

Replace cache-policy tests in `self_compose.rs` with source-mapping tests:

- current source cell renders current;
- stale source cell renders the cached headline with `openai_stale = true`;
- stale-without-value leaves the cell absent;
- resolver/provider error without cached data leaves the cell absent;
- `want_openai = false` does not call the source;
- Codex composition and staleness remain unchanged.

All TTL, lease, concurrency, cooldown, and persistence tests belong in `openai_client` after this task. Do not duplicate them in the renderer.

- [ ] **Step 2: Implement the source adapter**

`LiveCrossSources` retains the resolved owned key and API base URL. Its OpenAI method calls the gated API once. Map outcomes as follows:

| Gated outcome | Statusline cell |
|---|---|
| current full success, fetched or cached | total, current |
| recent in-flight/failure with cached total | cached total, stale |
| legacy headline | headline, stale |
| prior-month full success inside gate | total, stale |
| no key | absent, not stale |
| gate/provider error with no cached total | absent, not stale |

Log only a safe debug message for suppressed or failed self-compose. Do not log the key, fingerprint, response body, or store contents.

- [ ] **Step 3: Preserve the demand gate**

Update `statusline.rs` so `want_openai = false` avoids constructing the gated OpenAI source path. No cache directory lookup, keychain access, or API base resolution should occur. Remove direct references to `statusline_render::cache`.

- [ ] **Step 4: Update the subprocess integration tests**

Keep both existing invariants:

- explicit `{openai_cost}` across two statusline subprocesses yields one GET and renders `$4.20` twice;
- the shipped default template yields zero GETs and no OpenAI cost segment.

Add:

- a failed first subprocess followed by a second subprocess yields one GET total;
- an unwritable gate root yields zero GETs and the statusline still exits successfully;
- a different key does not reuse the first key's value;
- a legacy single-entry cache is displayed stale without a fetch inside its active gate.

- [ ] **Step 5: Remove moved dependencies**

`statusline_render` should no longer need `atomic_file`, `directories`, `serde`, or `serde_json` unless another module demonstrably uses them. Do not add `openai_client` to the renderer. Keep provider types in the CLI adapter and convert them to the renderer-owned source cell so the renderer remains presentation-focused.

- [ ] **Step 6: Run the task gate**

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo nextest run -p openai_client -p statusline_render -p balanze_cli
  cargo nextest run --workspace
  cargo tree --workspace --duplicates
  ```

- [ ] **Step 7: Commit**

  ```bash
  git add crates/openai_client crates/statusline_render crates/balanze_cli/src/sources.rs crates/balanze_cli/src/statusline.rs crates/balanze_cli/tests/integration_statusline_self_compose.rs Cargo.lock
  git commit -m "refactor(statusline): consume the shared OpenAI costs gate"
  ```

## Task 4: Route one-shot status and export through the gate

The one-shot CLI paths already share `live_fetch_openai`; this task makes that helper gated and proves it reuses the full cached result.

**Files:**

- Modify: `crates/balanze_cli/src/sources.rs`
- Modify: `crates/balanze_cli/src/export.rs` only if error mapping needs caller-specific context
- Modify: `crates/balanze_cli/tests/integration_4quadrant.rs` only if typed error expectations change
- Add or modify an integration test under `crates/balanze_cli/tests/` for cross-command gating

- [ ] **Step 1: Add failing shared-path tests**

With one temporary cache root, key, and wiremock base URL, prove:

- a one-shot status fetch followed by export produces one GET total;
- export receives the full line-item result from the store, not only the headline total;
- status followed by statusline produces one GET total;
- export followed by statusline produces one GET total;
- a recent provider failure prevents all three paths from making a second GET;
- a prior-month cached result is not emitted as current export rows;
- different keys produce separate results with no line-item crossover.

Prefer testing the shared `live_fetch_openai` helper plus the existing statusline subprocess test over building a brittle end-to-end fixture for unrelated Anthropic sources.

- [ ] **Step 2: Use one base URL and one resolved key**

`live_fetch_openai` currently calls the constant `OPENAI_API_BASE`, while statusline uses `openai_api_base()`. Route both through the same normalized base resolver so the request fingerprint and HTTP target cannot diverge. Preserve `BALANZE_OPENAI_API_BASE` as a test seam.

- [ ] **Step 3: Map gate errors without losing typed provider guidance**

- Current full success returns `Ok(Some(OpenAiCosts))`.
- No configured key returns `Ok(None)` before touching the gate.
- 401/403, whether observed now or replayed from safe stored classification, use the shared admin-key guidance.
- In-flight, gate storage, 429, network, 5xx, response-shape, and month-rollover suppression remain retryable/degraded errors.
- Month-rollover suppression must identify the shared 5-minute gate and remaining retry time when available. It must not imply that the key is invalid or that the prior-month value is current.
- Export must not silently output an empty OpenAI section for a configured key whose request was gate-blocked by an error.

- [ ] **Step 4: Run the task gate**

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo nextest run -p balanze_cli
  cargo nextest run --workspace
  ```

- [ ] **Step 5: Commit**

  ```bash
  git add crates/balanze_cli/src/sources.rs crates/balanze_cli/src/export.rs crates/balanze_cli/tests
  git commit -m "fix(cli): share the OpenAI costs polling gate"
  ```

## Task 5: Route watcher polling and every key-validation path through the gate

This task closes the original watcher handoff in both directions and removes the remaining direct validation bypass.

**Files:**

- Modify: `crates/watcher/src/tasks/openai_poll.rs`
- Modify: `crates/watcher/src/validate.rs`
- Modify: `crates/balanze_cli/src/setup.rs`
- Verify only: `crates/balanze_cli/src/probes.rs`
- Verify only: `src-tauri/src/commands.rs`

- [ ] **Step 1: Extract a testable watcher poll operation**

Keep `spawn` responsible for interval and coordinator delivery. Extract one async operation that receives the already-resolved key, base URL, timeout, and fixed generation, calls the gated API, and returns the existing `SourceUpdate` shape.

The production loop still resolves the key once at task startup. Change the watcher request from 30s plus `BackoffPolicy::standard()` to one 30-second fail-fast request guarded by the durable 300-second reservation.

- [ ] **Step 2: Write watcher handoff tests**

Use wiremock and one temporary gate root:

- watcher poll success followed by the same gated call used by statusline -> one GET total and the same total;
- statusline-style call followed by watcher startup -> one GET total;
- watcher 401/403/429/500/timeout followed by statusline-style call -> one GET total;
- concurrent watcher-style and statusline-style calls -> one GET total;
- watcher completion write failure still emits its successful coordinator update while the persisted reservation suppresses a second GET;
- watcher gate reservation failure emits an OpenAI error update and performs zero GETs;
- a cached full success may populate the watcher without HTTP;
- a prior-month cached result does not populate the watcher as current.

These tests target the shared provider choke point through watcher behavior. Do not add provenance to `SourceUpdate`, `SourcePartial`, `Snapshot`, or the coordinator.

- [ ] **Step 3: Route reusable validation through the gate**

`watcher::validate_openai_key` uses the same gated public API. Map outcomes:

| Outcome | `KeyProbe` |
|---|---|
| fetched or cached current full success | `Valid` |
| current or stored 401/403 classification | `Rejected` |
| recent 429/network/5xx/shape failure | `Unreachable` |
| another request in flight | `Unreachable` |
| gate unavailable/full | `Unreachable` |
| prior-month/legacy value inside active gate | `Unreachable` |

Messages must say when validation is temporarily deferred by the shared 5-minute gate and that saving the key remains possible for retryable cases. This includes the accepted UTC month-rollover window: the old-month attempt stays gated even though its result cannot validate the new month. Never claim a blocked validation proves the key is invalid.

- [ ] **Step 4: Delete setup's duplicate HTTP validation**

Replace `balanze_cli::setup::validate_openai_key_blocking` internals with a local runtime call to `watcher::validate_openai_key`, then map `KeyProbe` to the setup wizard's existing `Result<()>` messages. `doctor` already delegates through watcher validation; Tauri `validate_api_key` already does too.

After this step, run:

  ```bash
  rg -n "costs_this_month_with\(|costs_this_month\(|fetch_costs\(" crates src-tauri --glob "*.rs" --glob "!crates/openai_client/**"
  ```

Expected: no matches. This grep is a required acceptance gate, not an informational check.

- [ ] **Step 5: Add validation behavior tests**

Cover fresh cached success, stored auth rejection, stored transient failure, in-flight request, gate I/O failure, full store, legacy headline, and prior-month result. Existing pure `classify` tests remain and gain cases for the gated error type.

- [ ] **Step 6: Run the task gate**

  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo nextest run -p openai_client -p watcher -p balanze_cli
  cargo nextest run --workspace
  ```

- [ ] **Step 7: Commit**

  ```bash
  git add crates/watcher/src/tasks/openai_poll.rs crates/watcher/src/validate.rs crates/balanze_cli/src/setup.rs
  git commit -m "fix(watcher): share the OpenAI costs polling gate"
  ```

## Task 6: Update contracts, user guidance, and release notes

Documentation must describe the provider-wide behavior, not preserve the former statusline-only architecture. This task records an intentional amendment to two existing non-negotiables, not a clarification of behavior that was already permitted.

**Files:**

- Modify: `AGENTS.md`
- Modify: `docs/ARCHITECTURE.md`
- Modify: `docs/TROUBLESHOOTING.md`
- Modify: `docs/src/cli.md`
- Modify: `docs/src/faq.md`
- Modify: `docs/PRD.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Update the operational contract**

In `AGENTS.md` section 3.1:

- explicitly state that this contract intentionally replaces the former OpenAI watcher retry rule and the former statusline-only cache ownership rule;
- define the gate across watcher, statusline, status, export, setup, doctor, and Tauri validation;
- replace the OpenAI half of the HTTP retry/backoff non-negotiable: all Costs requests are fail-fast, every attempted request durably reserves 300 seconds, and retry happens on a later eligible poll rather than inside one call;
- replace the statusline self-compose non-negotiable with provider ownership of the shared gate while retaining the statusline's `{openai_cost}` demand check, 3-second HTTP timeout, stale headline behavior, and no Anthropic OAuth call;
- remove every residual claim that the watcher uses `BackoffPolicy::standard()` for OpenAI or that only statusline processes coordinate through the cache;
- retain standard backoff for applicable Anthropic requests;
- describe the eight-entry fingerprinted store, one store-wide lease, durable reservation, token-checked completion, cache-root scope, month rollover, and fail-closed storage behavior;
- record the deliberate month-rollover tradeoff: a pre-boundary attempt can defer non-statusline callers for up to 300 seconds into the new UTC month, and the month boundary does not bypass the reservation;
- state the accepted limitation that explicit OpenAI organization selection is unsupported.

Update the logging table so OpenAI 429 is a warn/degraded event, not an immediate retry.

- [ ] **Step 2: Update architecture ownership and flow**

In `docs/ARCHITECTURE.md`:

- move cache/gate ownership from `statusline_render` to `openai_client` in the crate map;
- change the top data-flow diagram from a watcher-specific 5-minute poll to the shared provider gate;
- replace "Self-compose fallback cache" with a provider-wide "OpenAI Costs gate" section;
- include the reservation state diagram and caller outcome table;
- document that raw Costs HTTP is private to `openai_client`;
- document the unchanged actor boundary: the watcher still sends ordinary `SourceUpdate<OpenAiCosts>` after a gated result;
- document the legacy single-entry migration and mixed-version limitation;
- keep the statusline demand gate and 250ms contention behavior.

- [ ] **Step 3: Update user-facing behavior**

Explain:

- a recent status, export, watcher, or validation request can satisfy or temporarily defer another Costs consumer;
- validation may say "try again after the shared 5-minute window" without meaning the key is invalid;
- for up to 5 minutes after a UTC month boundary, a just-completed prior-month attempt can leave status, export, watcher, and validation temporarily unavailable while the literal reservation remains active;
- a stale statusline value can remain visible while other surfaces report the source as unavailable;
- the guarantee is scoped to one user and one cache root;
- the default statusline still makes no OpenAI call because `{openai_cost}` is off by default.

Do not expose fingerprints, reservation tokens, lease filenames, or internal schema fields in user-facing docs.

- [ ] **Step 4: Update milestone and changelog text**

Mark issue #204's provenance-safe cache seeding item as the stronger provider-wide gate. Record:

- watcher-to-statusline duplicate polling fixed;
- reverse startup and failure handoffs fixed;
- one-shot status/export/validation now share the same gate;
- cached spend cannot cross request fingerprints;
- OpenAI Costs retries now happen on a later 300-second poll, not inside one logical attempt.

- [ ] **Step 5: Scan documentation hygiene**

  ```bash
  rg -n "openai-cost.json|standard\(\).*OpenAI|OpenAI 429 retry|statusline-only cache|60s failure" README.md AGENTS.md docs CHANGELOG.md --glob "*.md" --glob "!docs/superpowers/plans/**" --glob "!docs/superpowers/specs/**"
  rg -n -P "\x{2014}|\x{2013}|\x{2026}" AGENTS.md docs CHANGELOG.md
  ```

The first command may find the retained cache filename, but every surrounding claim must describe provider-wide ownership. The punctuation command must return no matches in modified prose.

- [ ] **Step 6: Run the full gate**

  ```bash
  cargo build --workspace
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo nextest run --workspace
  cargo tree --workspace --duplicates
  bun run check
  mdbook build
  ```

- [ ] **Step 7: Commit**

  ```bash
  git add AGENTS.md docs/ARCHITECTURE.md docs/TROUBLESHOOTING.md docs/src/cli.md docs/src/faq.md docs/PRD.md CHANGELOG.md
  git commit -m "docs(openai): document the machine-wide costs gate"
  ```

## Final Verification

- [ ] **Invariant-focused tests**

  ```bash
  cargo nextest run -p openai_client gate
  cargo nextest run -p openai_client --test wiremock_tests
  cargo nextest run -p watcher openai
  cargo nextest run -p balanze_cli statusline
  ```

- [ ] **Required workspace gates**

  ```bash
  cargo build --workspace
  cargo clippy --workspace --all-targets -- -D warnings
  cargo nextest run --workspace
  cargo fmt --all -- --check
  bun run check
  ```

- [ ] **No bypass grep**

  ```bash
  rg -n "costs_this_month_with\(|costs_this_month\(|fetch_costs\(" crates src-tauri --glob "*.rs" --glob "!crates/openai_client/**"
  ```

Expected: no matches.

- [ ] **Secret and schema inspection**

Use an isolated test cache and inspect the serialized gate document:

- no raw OpenAI key;
- no raw API base URL;
- no response body;
- no canonical `Snapshot` or settings changes;
- at most eight entries;
- full `OpenAiCosts` only under its matching request fingerprint;
- legacy root projection contains only the non-secret key fingerprint, headline value, and timestamps.

- [ ] **Manual real-endpoint check**

On the developer machine with a real configured Admin key:

1. Run one command that requests OpenAI Costs.
2. Within 300 seconds, run status, export, and explicit key validation.
3. Confirm logs show no additional Costs request and every surface receives either the cached full result or the documented deferred result.
4. Stop the watcher after a successful poll, wait until `snapshot.json` is stale but less than 300 seconds from the poll, and invoke a statusline configured with `{openai_cost}`.
5. Confirm the statusline uses the shared value without another request.
6. Repeat with a forced provider failure and confirm the statusline does not retry inside the gate.

Do not log or paste the key while performing this check.

- [ ] **Manual desktop smoke**

Because watcher Rust changes reach `src-tauri` transitively:

  ```bash
  bun run tauri dev
  ```

Confirm the tray appears, OpenAI populates from either a gated fetch or current cached result, opening the popover works, and Quit exits cleanly.

- [ ] **Open the PR**

Use one branch and one PR because partial rollout leaves bypasses:

```bash
git switch -c fix/openai-costs-machine-gate
```

Suggested PR title:

```text
fix(openai): enforce one machine-wide Costs poll gate
```

The PR body should link issue #204 and call out the literal retry change, multi-entry migration, fail-closed storage rule, and mixed-version limitation.

## Acceptance Matrix

| Scenario | Expected HTTP count inside 300 seconds | Expected value behavior |
|---|---:|---|
| watcher success -> statusline | 1 | same-key value reused |
| statusline success -> watcher startup | 1 | watcher receives cached full result |
| status -> export -> validation | 1 | full current-month result reused |
| watcher failure -> statusline | 1 | stale headline or absent cell, no retry |
| 429 with short `Retry-After` | 1 | retryable degraded result, no in-call retry |
| concurrent same-key processes | 1 | one owner, contenders reuse or defer |
| owner crashes after reservation | 1 | request suppressed until reservation expires |
| completion arrives after successor | unchanged | late completion ignored |
| reservation store unwritable | 0 | fail closed and degrade |
| completion store unwritable | 1 | current caller keeps result; reservation remains |
| different key | 1 per fingerprint | values remain separate |
| same key, different API base | 1 per fingerprint | values remain separate |
| attempt just before UTC month rollover | 0 until its 300 seconds expire | statusline may show prior-month stale; status, export, watcher, and validation deliberately defer |
| legacy headline inside gate | 0 | statusline stale only; no invented full result |
| ninth identity while eight are active | 0 for ninth | gate-full degraded result |
| statusline without `{openai_cost}` | 0 | OpenAI path remains untouched |

## Out of Scope

- Explicit OpenAI organization selection or organization-header support.
- A canonical `Snapshot` provenance field or `SnapshotFilePayload` schema bump.
- Frontend IPC, CLI JSON, settings, or coordinator message changes.
- Durable billing history across calendar months.
- A new general-purpose cross-provider request scheduler.
- Strict coordination with indefinitely running pre-change binaries.
- Guarantees across different OS users, different cache roots, containers, or machines.
- Protection against a manual wall-clock jump forward by 300 seconds or more.
