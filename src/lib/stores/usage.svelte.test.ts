import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { Snapshot } from '../types/snapshot';

// Mock the IPC layer the store depends on. `refresh()` must pull via
// getSnapshot (the self-healing path), NOT re-emit through refresh_now: a
// regression to the event-only path is exactly the freeze this guards against.
const getSnapshot = vi.fn<() => Promise<Snapshot>>();
const onUsageUpdated = vi.fn(async (_cb: (s: Snapshot) => void) => () => {});
const onDegraded = vi.fn(async (_cb: (d: { source: string; error: string }) => void) => () => {});
const onWindowShown = vi.fn(async (_cb: () => void) => () => {});
vi.mock('../ipc', () => ({
  getSnapshot: () => getSnapshot(),
  onUsageUpdated: (cb: (s: Snapshot) => void) => onUsageUpdated(cb),
  onDegraded: (cb: (d: { source: string; error: string }) => void) => onDegraded(cb),
  onWindowShown: (cb: () => void) => onWindowShown(cb),
}));

import { usage } from './usage.svelte';

function snapshotWith(error: string | null = null): Snapshot {
  return {
    fetched_at: '2026-06-17T00:00:00Z',
    claude_oauth: null,
    claude_oauth_error: error,
    claude_jsonl: null,
    claude_jsonl_error: null,
    anthropic_api_cost: null,
    anthropic_api_cost_error: null,
    codex_quota: null,
    codex_quota_error: null,
    openai: null,
    openai_error: null,
    claude_statusline: null,
    claude_statusline_error: null,
    pace: [],
  } as unknown as Snapshot;
}

describe('UsageStore.refresh', () => {
  beforeEach(() => {
    getSnapshot.mockReset();
    onUsageUpdated.mockReset();
    onUsageUpdated.mockImplementation(async (_cb: (s: Snapshot) => void) => () => {});
    onDegraded.mockReset();
    onDegraded.mockImplementation(
      async (_cb: (d: { source: string; error: string }) => void) => () => {},
    );
    onWindowShown.mockReset();
    onWindowShown.mockImplementation(async (_cb: () => void) => () => {});
    usage.destroy();
    usage.snapshot = null;
    usage.degraded = {};
    usage.loading = true;
    usage.lastError = null;
  });

  it('updates the snapshot directly from getSnapshot (not via the event channel)', async () => {
    const s = snapshotWith();
    getSnapshot.mockResolvedValue(s);

    await usage.refresh();

    expect(getSnapshot).toHaveBeenCalledOnce();
    expect(usage.snapshot).toBe(s);
  });

  it('reconciles degraded markers from the refreshed snapshot', async () => {
    getSnapshot.mockResolvedValue(snapshotWith('AuthExpired'));

    await usage.refresh();

    expect(usage.degraded.claude_oauth).toBe('AuthExpired');

    // A clean refresh clears the marker rather than leaving it stuck.
    getSnapshot.mockResolvedValue(snapshotWith(null));
    await usage.refresh();
    expect(usage.degraded.claude_oauth).toBeUndefined();
  });

  it('records the error and leaves the store usable when getSnapshot rejects', async () => {
    getSnapshot.mockRejectedValue(new Error('IPC down'));

    await usage.refresh();

    expect(usage.lastError).toContain('IPC down');
  });

  it('surfaces listener registration failure even when initial snapshot succeeds', async () => {
    const s = snapshotWith(null);
    onUsageUpdated.mockRejectedValueOnce(new Error('listen down'));
    getSnapshot.mockResolvedValue(s);

    await usage.init();

    expect(usage.snapshot).toBe(s);
    expect(usage.lastError).toContain('listen down');
    expect(usage.degraded.frontend_events).toContain('listen down');
  });

  it('cleans up partial listener registrations when init listener setup fails', async () => {
    const unlistenUsage = vi.fn();
    onUsageUpdated.mockResolvedValueOnce(unlistenUsage);
    onDegraded.mockRejectedValueOnce(new Error('degraded listen down'));
    getSnapshot.mockResolvedValue(snapshotWith(null));

    await usage.init();

    expect(unlistenUsage).toHaveBeenCalledOnce();
    expect(onWindowShown).not.toHaveBeenCalled();
    expect(usage.degraded.frontend_events).toContain('degraded listen down');

    usage.destroy();
    expect(unlistenUsage).toHaveBeenCalledOnce();
  });

  it('clears the public frontend event marker on destroy', () => {
    usage.degraded = {
      claude_oauth: 'AuthExpired',
      frontend_events: 'listen down',
    };

    usage.destroy();

    expect(usage.degraded.frontend_events).toBeUndefined();
    expect(usage.degraded.claude_oauth).toBe('AuthExpired');
  });
});
