import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { openUrl } from '@tauri-apps/plugin-opener';
import type { Snapshot, DegradedPayload } from './types/snapshot';
import type { Settings, StatuslineWire } from './types/settings';

export const getSnapshot = (): Promise<Snapshot> => invoke<Snapshot>('get_snapshot');
export const refreshNow = (): Promise<void> => invoke<void>('refresh_now');

// Hide the popover window (ESC-to-dismiss; mirrors the blur-hide behavior).
// Goes through an app command rather than `getCurrentWindow().hide()` so it
// needs no `core:window` capability - Rust owns window manipulation.
export const hideWindow = (): Promise<void> => invoke<void>('hide_window');

// Ask the host to resize the popover window to `height` logical px and re-anchor
// it. Like `hide_window`, this goes through an app command so the webview needs
// no `core:window` capability.
export const resizePopover = (height: number): Promise<void> =>
  invoke<void>('resize_popover', { height });

// Launch-at-login (autostart). Goes through Rust app commands that drive the
// `tauri-plugin-autostart` plugin, so the webview needs no `autostart:`
// capability. The OS login-item state is the source of truth (`get` reads it
// live on mount), so there is no `settings.json` flag to keep in sync.
export const getLaunchAtLogin = (): Promise<boolean> =>
  invoke<boolean>('get_launch_at_login');
export const setLaunchAtLogin = (enabled: boolean): Promise<void> =>
  invoke<void>('set_launch_at_login', { enabled });

// Non-secret settings (settings.json shape). `get_settings` never returns any
// API key; `set_api_key` writes the key to the OS keychain and flips the
// provider's enable flag backend-side (AGENTS.md §3.4).
export const getSettings = (): Promise<Settings> => invoke<Settings>('get_settings');
export const setSettings = (settings: Settings): Promise<void> =>
  invoke<void>('set_settings', { settings });
export const setApiKey = (provider: string, key: string): Promise<void> =>
  invoke<void>('set_api_key', { provider, key });
export const hasApiKey = (provider: string): Promise<boolean> =>
  invoke<boolean>('has_api_key', { provider });
export const clearApiKey = (provider: string): Promise<void> =>
  invoke<void>('clear_api_key', { provider });

// Probe a key against the provider WITHOUT storing it (so Settings can give
// immediate feedback instead of waiting a poll interval). `ok` = authenticated;
// `retryable` = the check failed transiently (network / rate limit) so the UI
// may offer "save anyway"; a non-retryable failure means the key is wrong.
export interface ApiKeyValidation {
  ok: boolean;
  retryable: boolean;
  message: string | null;
}
export const validateApiKey = (provider: string, key: string): Promise<ApiKeyValidation> =>
  invoke<ApiKeyValidation>('validate_api_key', { provider, key });

// Open an external URL in the user's default browser via the opener plugin
// (the `opener:default` capability is already granted to the main window).
export const openExternal = (url: string): Promise<void> => openUrl(url);

// Claude Code statusLine wiring (delegates to claude_statusline backend-side;
// no-clobber - won't overwrite another tool's statusLine).
export const getStatuslineStatus = (): Promise<StatuslineWire> =>
  invoke<StatuslineWire>('get_statusline_status');
export const setStatuslineWired = (wired: boolean): Promise<void> =>
  invoke<void>('set_statusline_wired', { wired });
export const replaceStatusline = (): Promise<void> => invoke<void>('replace_statusline');
export const restoreStatusline = (): Promise<void> => invoke<void>('restore_statusline');

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
