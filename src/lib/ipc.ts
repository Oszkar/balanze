import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import type { Snapshot, DegradedPayload } from './types/snapshot';

export const getSnapshot = (): Promise<Snapshot> => invoke<Snapshot>('get_snapshot');
export const refreshNow = (): Promise<void> => invoke<void>('refresh_now');

// Fires each time the popover window gains focus, i.e. every tray-click show
// (the backend shows via `window.show()` + `set_focus()`; blur hides it). The
// store uses this to re-pull the snapshot on open, so the popover is fresh on
// every open and self-heals if the `usage_updated` event channel ever dies
// (e.g. a webview reload orphaning the listener) - the freeze that otherwise
// only the next live emit could clear.
export const onWindowShown = (cb: () => void): Promise<UnlistenFn> =>
  getCurrentWindow().onFocusChanged(({ payload: focused }) => {
    if (focused) cb();
  });

export const onUsageUpdated = (cb: (s: Snapshot) => void): Promise<UnlistenFn> =>
  listen<Snapshot>('usage_updated', (e) => cb(e.payload));

export const onDegraded = (cb: (d: DegradedPayload) => void): Promise<UnlistenFn> =>
  listen<DegradedPayload>('degraded_state', (e) => cb(e.payload));
