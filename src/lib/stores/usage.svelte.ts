import type { UnlistenFn } from '@tauri-apps/api/event';
import { getSnapshot, onUsageUpdated, onDegraded, onWindowShown } from '../ipc';
import type { Snapshot } from '../types/snapshot';

// The authoritative degraded state is the snapshot's per-source `*_error`
// slots (the coordinator sets them on failure and clears them on a source's
// next success). Deriving the map from each snapshot means a recovered source
// clears its marker, and a snapshot fetched with pre-existing errors is
// reflected - rather than only ever appending `degraded_state` events, which
// never clear (the bug Codex flagged).
function degradedFromSnapshot(s: Snapshot): Record<string, string> {
  const d: Record<string, string> = {};
  if (s.claude_oauth_error) d.claude_oauth = s.claude_oauth_error;
  if (s.claude_jsonl_error) d.claude_jsonl = s.claude_jsonl_error;
  if (s.anthropic_api_cost_error) d.anthropic_api_cost = s.anthropic_api_cost_error;
  if (s.codex_quota_error) d.codex_quota = s.codex_quota_error;
  if (s.openai_error) d.openai_costs = s.openai_error;
  if (s.claude_statusline_error) d.claude_statusline = s.claude_statusline_error;
  return d;
}

// Inverse of `degradedFromSnapshot`: which Snapshot error slot a
// `degraded_state` source key belongs to. An error-only transition publishes
// the snapshot to durable storage and emits `degraded_state` WITHOUT a
// `usage_updated` (the coordinator's `on_snapshot_durable` path), so the
// frontend's copy of the snapshot keeps a null error slot. Every cell derives
// its error/stale presentation from those slots, not from `degraded`, so
// without reconciling the event back into the snapshot the quota keeps its
// fresh appearance through repeated failures while only the banner warns.
const SNAPSHOT_ERROR_SLOT = {
  claude_oauth: 'claude_oauth_error',
  claude_jsonl: 'claude_jsonl_error',
  anthropic_api_cost: 'anthropic_api_cost_error',
  codex_quota: 'codex_quota_error',
  openai_costs: 'openai_error',
  claude_statusline: 'claude_statusline_error',
} as const satisfies Record<string, keyof Snapshot>;

type DegradedSource = keyof typeof SNAPSHOT_ERROR_SLOT;

class UsageStore {
  snapshot = $state<Snapshot | null>(null);
  degraded = $state<Record<string, string>>({});
  loading = $state(true);
  lastError = $state<string | null>(null);
  #unlisten: UnlistenFn[] = [];
  #frontendEventError: string | null = null;
  #destroyed = false;
  #lifecycle = 0;
  #eventRevision = 0;
  #requestSequence = 0;
  #latestRequest = 0;

  #applySnapshot(s: Snapshot, fromEvent = false) {
    this.snapshot = s;
    const degraded = degradedFromSnapshot(s);
    if (this.#frontendEventError) degraded.frontend_events = this.#frontendEventError;
    this.degraded = degraded;
    if (fromEvent) this.#eventRevision += 1;
  }

  // Reconcile an error-only `degraded_state` into the snapshot's error slot so
  // the cells react, not just the banner. `frontend_events` has no slot (it is
  // not a backend source) and is skipped. A later `usage_updated` replaces the
  // snapshot wholesale from the backend, which is what clears the slot once
  // the source recovers - this never becomes a sticky client-side marker.
  #applyDegradedToSnapshot(source: string, error: string) {
    const slot = SNAPSHOT_ERROR_SLOT[source as DegradedSource];
    if (!slot || !this.snapshot || this.snapshot[slot] === error) return;
    this.snapshot = { ...this.snapshot, [slot]: error };
  }

  #recordFrontendEventError(e: unknown) {
    const msg = String(e);
    this.lastError = msg;
    this.#frontendEventError = msg;
    this.degraded = { ...this.degraded, frontend_events: msg };
    this.#eventRevision += 1;
  }

  #unlistenAll(unlistenFns: UnlistenFn[]) {
    for (const u of unlistenFns) {
      try {
        u();
      } catch {
        // Best-effort cleanup: a failed unlisten should not mask the original
        // listener registration failure or prevent the remaining handlers from
        // being removed.
      }
    }
  }

  async init() {
    const lifecycle = ++this.#lifecycle;
    this.#destroyed = false;
    // Register listeners BEFORE the initial fetch so a live emit during init
    // can't be lost (the OpenAI-only startup race: a `usage_updated` fired
    // between fetch and listen would be missed). Guarded separately: outside
    // the Tauri runtime (e.g. the page opened in a plain browser), `listen()`
    // rejects - record it rather than throwing an uncaught promise rejection.
    const pendingUnlisten: UnlistenFn[] = [];
    try {
      pendingUnlisten.push(await onUsageUpdated((s) => {
        if (this.#destroyed || lifecycle !== this.#lifecycle) return;
        // Reconcile from the snapshot's error slots so recovered sources clear.
        this.#applySnapshot(s, true);
      }));
      if (this.#destroyed || lifecycle !== this.#lifecycle) {
        this.#unlistenAll(pendingUnlisten);
        return;
      }
      pendingUnlisten.push(await onDegraded((d) => {
        if (this.#destroyed || lifecycle !== this.#lifecycle) return;
        // Immediate marker for a failure that didn't ride a snapshot (the
        // coordinator emits degraded_state without a usage_updated on error).
        this.degraded = { ...this.degraded, [d.source]: d.error };
        this.#applyDegradedToSnapshot(d.source, d.error);
        this.#eventRevision += 1;
      }));
      if (this.#destroyed || lifecycle !== this.#lifecycle) {
        this.#unlistenAll(pendingUnlisten);
        return;
      }
      // Re-pull on every popover open: fresh-on-open, and self-healing if the
      // live event channel above ever dies (a webview reload orphans the
      // listener; without this the UI would stay frozen until the next emit
      // that happens to land on a live listener - which never comes).
      pendingUnlisten.push(await onWindowShown(() => {
        if (!this.#destroyed && lifecycle === this.#lifecycle) void this.refresh();
      }));
      if (this.#destroyed || lifecycle !== this.#lifecycle) {
        this.#unlistenAll(pendingUnlisten);
        return;
      }
      this.#unlisten.push(...pendingUnlisten);
      this.#frontendEventError = null;
    } catch (e) {
      this.#unlistenAll(pendingUnlisten);
      if (!this.#destroyed && lifecycle === this.#lifecycle) this.#recordFrontendEventError(e);
    }
    if (this.#destroyed || lifecycle !== this.#lifecycle) return;

    // Seed first paint. A live event that arrives during the await advances the
    // event revision, so the older response is discarded instead of wiping it.
    try {
      await this.#refreshSnapshot();
    } catch (e) {
      if (!this.#destroyed && lifecycle === this.#lifecycle) this.lastError = String(e);
    } finally {
      if (!this.#destroyed && lifecycle === this.#lifecycle) this.loading = false;
    }
  }

  // Pull the current snapshot straight from the backend and update the store,
  // rather than asking the backend to re-emit `usage_updated` (which the old
  // path did via refresh_now - that only worked if the event listener was
  // still live, and re-sent the *cached* snapshot anyway). `get_snapshot`
  // returns the coordinator's current snapshot, kept fresh by the pollers, so
  // this both repaints reliably and doesn't depend on the event channel.
  async refresh() {
    await this.#refreshSnapshot();
  }

  async #refreshSnapshot() {
    const lifecycle = this.#lifecycle;
    const eventRevision = this.#eventRevision;
    const request = ++this.#requestSequence;
    this.#latestRequest = request;
    try {
      const snapshot = await getSnapshot();
      // A live event is newer than the request's seed point, and the most recent
      // pull supersedes any older in-flight pull. Either condition makes this
      // response stale even if its IPC call happened to finish last.
      if (
        lifecycle === this.#lifecycle
        && eventRevision === this.#eventRevision
        && request === this.#latestRequest
      ) {
        this.#applySnapshot(snapshot);
        if (!this.#frontendEventError) this.lastError = null;
      }
    } catch (e) {
      if (lifecycle === this.#lifecycle && request === this.#latestRequest) {
        this.lastError = String(e);
      }
    }
  }

  destroy() {
    this.#destroyed = true;
    this.#lifecycle += 1;
    this.#eventRevision += 1;
    this.#latestRequest = ++this.#requestSequence;
    this.#unlistenAll(this.#unlisten);
    this.#unlisten = [];
    this.#frontendEventError = null;
    if ('frontend_events' in this.degraded) {
      const degraded = { ...this.degraded };
      delete degraded.frontend_events;
      this.degraded = degraded;
    }
  }
}

export const usage = new UsageStore();
