import { describe, it, expect, vi, beforeEach } from 'vitest';
import type { Snapshot } from '../types/snapshot';

// Mock the IPC layer the store depends on. `refresh()` must pull via
// getSnapshot (the self-healing path), NOT re-emit through refresh_now: a
// regression to the event-only path is exactly the freeze this guards against.
const getSnapshot = vi.fn<() => Promise<Snapshot>>();
let usageUpdatedCallback: ((s: Snapshot) => void) | null = null;
const onUsageUpdated = vi.fn(async (cb: (s: Snapshot) => void) => {
  usageUpdatedCallback = cb;
  return () => {};
});
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
    usageUpdatedCallback = null;
    onUsageUpdated.mockReset();
    onUsageUpdated.mockImplementation(async (cb: (s: Snapshot) => void) => {
      usageUpdatedCallback = cb;
      return () => {};
    });
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

  it('does not let the initial getSnapshot response overwrite a newer live event', async () => {
    let resolveSnapshot!: (snapshot: Snapshot) => void;
    getSnapshot.mockImplementation(
      () => new Promise<Snapshot>((resolve) => { resolveSnapshot = resolve; }),
    );
    const init = usage.init();
    await vi.waitFor(() => expect(usageUpdatedCallback).not.toBeNull());
    await vi.waitFor(() => expect(getSnapshot).toHaveBeenCalledOnce());

    const newer = snapshotWith('newer event error');
    usageUpdatedCallback!(newer);
    resolveSnapshot(snapshotWith(null));
    await init;

    expect(usage.snapshot).toBe(newer);
    expect(usage.degraded.claude_oauth).toBe('newer event error');
  });

  it('does not let a refresh response overwrite a newer live event', async () => {
    getSnapshot.mockResolvedValueOnce(snapshotWith(null));
    await usage.init();

    let resolveRefresh!: (snapshot: Snapshot) => void;
    getSnapshot.mockImplementationOnce(
      () => new Promise<Snapshot>((resolve) => { resolveRefresh = resolve; }),
    );
    const refresh = usage.refresh();
    await vi.waitFor(() => expect(getSnapshot).toHaveBeenCalledTimes(2));

    const newer = snapshotWith('newer refresh event error');
    usageUpdatedCallback!(newer);
    resolveRefresh(snapshotWith(null));
    await refresh;

    expect(usage.snapshot).toBe(newer);
    expect(usage.degraded.claude_oauth).toBe('newer refresh event error');
  });

  it('does not let an older overlapping refresh overwrite the newest request', async () => {
    let resolveFirst!: (snapshot: Snapshot) => void;
    let resolveSecond!: (snapshot: Snapshot) => void;
    getSnapshot
      .mockImplementationOnce(
        () => new Promise<Snapshot>((resolve) => { resolveFirst = resolve; }),
      )
      .mockImplementationOnce(
        () => new Promise<Snapshot>((resolve) => { resolveSecond = resolve; }),
      );

    const first = usage.refresh();
    const second = usage.refresh();
    await vi.waitFor(() => expect(getSnapshot).toHaveBeenCalledTimes(2));

    const newest = snapshotWith('newest request error');
    resolveSecond(newest);
    await second;
    resolveFirst(snapshotWith('older request error'));
    await first;

    expect(usage.snapshot).toBe(newest);
    expect(usage.degraded.claude_oauth).toBe('newest request error');
  });

  it('ignores an older overlapping refresh rejection after a newer success', async () => {
    let rejectFirst!: (error: Error) => void;
    let resolveSecond!: (snapshot: Snapshot) => void;
    getSnapshot
      .mockImplementationOnce(
        () => new Promise<Snapshot>((_resolve, reject) => { rejectFirst = reject; }),
      )
      .mockImplementationOnce(
        () => new Promise<Snapshot>((resolve) => { resolveSecond = resolve; }),
      );

    const first = usage.refresh();
    const second = usage.refresh();
    const newest = snapshotWith(null);
    resolveSecond(newest);
    await second;
    rejectFirst(new Error('stale failure'));
    await first;

    expect(usage.snapshot).toBe(newest);
    expect(usage.lastError).toBeNull();
  });

  it('unlistens registrations that finish after destroy', async () => {
    const unlistenUsage = vi.fn();
    const unlistenDegraded = vi.fn();
    let resolveDegraded!: (unlisten: typeof unlistenDegraded) => void;
    onUsageUpdated.mockResolvedValueOnce(unlistenUsage);
    onDegraded.mockImplementationOnce(
      () => new Promise<(typeof unlistenDegraded)>((resolve) => { resolveDegraded = resolve; }),
    );

    const init = usage.init();
    await vi.waitFor(() => expect(onDegraded).toHaveBeenCalledOnce());
    usage.destroy();
    resolveDegraded(unlistenDegraded);
    await init;

    expect(unlistenUsage).toHaveBeenCalledOnce();
    expect(unlistenDegraded).toHaveBeenCalledOnce();
    expect(onWindowShown).not.toHaveBeenCalled();
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
