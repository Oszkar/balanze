import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { Snapshot, DegradedPayload } from './types/snapshot';

export const getSnapshot = (): Promise<Snapshot> => invoke<Snapshot>('get_snapshot');
export const refreshNow = (): Promise<void> => invoke<void>('refresh_now');

export const onUsageUpdated = (cb: (s: Snapshot) => void): Promise<UnlistenFn> =>
  listen<Snapshot>('usage_updated', (e) => cb(e.payload));

export const onDegraded = (cb: (d: DegradedPayload) => void): Promise<UnlistenFn> =>
  listen<DegradedPayload>('degraded_state', (e) => cb(e.payload));
