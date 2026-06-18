import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { Snapshot } from '../types/snapshot';

// Mock the IPC layer the store depends on. `refresh()` must pull via
// getSnapshot (the self-healing path), NOT re-emit through refresh_now: a
// regression to the event-only path is exactly the freeze this guards against.
const getSnapshot = vi.fn<() => Promise<Snapshot>>();
vi.mock('../ipc', () => ({
  getSnapshot: () => getSnapshot(),
  onUsageUpdated: vi.fn(async () => () => {}),
  onDegraded: vi.fn(async () => () => {}),
  onWindowShown: vi.fn(async () => () => {}),
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
});
