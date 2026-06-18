import type { UnlistenFn } from '@tauri-apps/api/event';
import { getSnapshot, onUsageUpdated, onDegraded, onWindowShown } from '../ipc';
import type { Snapshot } from '../types/snapshot';

// The authoritative degraded state is the snapshot's per-source `*_error`
// slots (the coordinator sets them on failure and clears them on a source's
// next success). Deriving the map from each snapshot means a recovered source
// clears its marker, and a snapshot fetched with pre-existing errors is
// reflected — rather than only ever appending `degraded_state` events, which
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

class UsageStore {
  snapshot = $state<Snapshot | null>(null);
  degraded = $state<Record<string, string>>({});
  loading = $state(true);
  lastError = $state<string | null>(null);
  #unlisten: UnlistenFn[] = [];

  async init() {
    // Register listeners BEFORE the initial fetch so a live emit during init
    // can't be lost (the OpenAI-only startup race: a `usage_updated` fired
    // between fetch and listen would be missed). Guarded separately: outside
    // the Tauri runtime (e.g. the page opened in a plain browser), `listen()`
    // rejects — record it rather than throwing an uncaught promise rejection.
    try {
      this.#unlisten.push(await onUsageUpdated((s) => {
        this.snapshot = s;
        // Reconcile from the snapshot's error slots so recovered sources clear.
        this.degraded = degradedFromSnapshot(s);
      }));
      this.#unlisten.push(await onDegraded((d) => {
        // Immediate marker for a failure that didn't ride a snapshot (the
        // coordinator emits degraded_state without a usage_updated on error).
        this.degraded = { ...this.degraded, [d.source]: d.error };
      }));
      // Re-pull on every popover open: fresh-on-open, and self-healing if the
      // live event channel above ever dies (a webview reload orphans the
      // listener; without this the UI would stay frozen until the next emit
      // that happens to land on a live listener - which never comes).
      this.#unlisten.push(await onWindowShown(() => void this.refresh()));
    } catch (e) {
      this.lastError = String(e);
    }

    // Seed first paint. A late-arriving live emit overwrites this; an emit
    // that arrives during the await is already captured by the listeners above.
    try {
      const s = await getSnapshot();
      this.snapshot = s;
      this.degraded = degradedFromSnapshot(s);
    } catch (e) {
      this.lastError = String(e);
    } finally {
      this.loading = false;
    }
  }

  // Pull the current snapshot straight from the backend and update the store,
  // rather than asking the backend to re-emit `usage_updated` (which the old
  // path did via refresh_now - that only worked if the event listener was
  // still live, and re-sent the *cached* snapshot anyway). `get_snapshot`
  // returns the coordinator's current snapshot, kept fresh by the pollers, so
  // this both repaints reliably and doesn't depend on the event channel.
  async refresh() {
    try {
      const s = await getSnapshot();
      this.snapshot = s;
      this.degraded = degradedFromSnapshot(s);
    } catch (e) {
      this.lastError = String(e);
    }
  }

  destroy() {
    for (const u of this.#unlisten) u();
    this.#unlisten = [];
  }
}

export const usage = new UsageStore();
