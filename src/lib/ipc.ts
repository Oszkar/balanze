import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import type { Snapshot, DegradedPayload } from './types/snapshot';
import type { Settings, StatuslineWire } from './types/settings';

export const getSnapshot = (): Promise<Snapshot> => invoke<Snapshot>('get_snapshot');
export const refreshNow = (): Promise<void> => invoke<void>('refresh_now');

// Non-secret settings (settings.json shape). `get_settings` never returns any
// API key; `set_api_key` writes the key to the OS keychain and flips the
// provider's enable flag backend-side (AGENTS.md §3.4).
export const getSettings = (): Promise<Settings> => invoke<Settings>('get_settings');
export const setSettings = (settings: Settings): Promise<void> =>
  invoke<void>('set_settings', { settings });
export const setApiKey = (provider: string, key: string): Promise<void> =>
  invoke<void>('set_api_key', { provider, key });

// Claude Code statusLine wiring (delegates to claude_statusline backend-side;
// no-clobber - won't overwrite another tool's statusLine).
export const getStatuslineStatus = (): Promise<StatuslineWire> =>
  invoke<StatuslineWire>('get_statusline_status');
export const setStatuslineWired = (wired: boolean): Promise<void> =>
  invoke<void>('set_statusline_wired', { wired });

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
