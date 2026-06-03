import type { UnlistenFn } from '@tauri-apps/api/event';
import { getSnapshot, refreshNow, onUsageUpdated, onDegraded } from '../ipc';
import type { Snapshot } from '../types/snapshot';

class UsageStore {
  snapshot = $state<Snapshot | null>(null);
  degraded = $state<Record<string, string>>({});
  loading = $state(true);
  lastError = $state<string | null>(null);
  #unlisten: UnlistenFn[] = [];

  async init() {
    try {
      this.snapshot = await getSnapshot();
    } catch (e) {
      this.lastError = String(e);
    } finally {
      this.loading = false;
    }

    // Listener registration is guarded separately: outside the Tauri runtime
    // (e.g. the page opened in a plain browser), `listen()` rejects. Record it
    // rather than throwing an uncaught promise rejection.
    try {
      this.#unlisten.push(await onUsageUpdated((s) => {
        this.snapshot = s;
      }));
      this.#unlisten.push(await onDegraded((d) => {
        this.degraded = { ...this.degraded, [d.source]: d.error };
      }));
    } catch (e) {
      this.lastError = String(e);
    }
  }

  async refresh() {
    try { await refreshNow(); } catch (e) { this.lastError = String(e); }
  }

  destroy() {
    for (const u of this.#unlisten) u();
    this.#unlisten = [];
  }
}

export const usage = new UsageStore();
