import { describe, it, expect } from 'vitest';
import { quotaTone, anthropicQuota, codexElapsedFraction, codexWindowExpired, codexQuota, classifyOverage, overageCell } from './quota';
import type { Snapshot, ExtraUsage } from '../types/snapshot';

const base: Snapshot = {
  schema_version: 2,
  fetched_at: '2026-06-03T12:00:00Z',
  claude_oauth: null, claude_oauth_error: null, claude_oauth_unavailable: null,
  claude_jsonl: null, claude_jsonl_error: null,
  anthropic_api_cost: null, anthropic_api_cost_error: null,
  codex_quota: null, codex_quota_error: null,
  openai: null, openai_error: null,
  claude_statusline: null, claude_statusline_error: null,
  pace: [],
};

describe('quota', () => {
  it('keeps Grid and Cards on the cross-surface rounded threshold table', () => {
    // Both frontend views receive their tone from quotaTone. These cases match
    // the tray, CLI, TUI, and statusline parity tables at every rounded cutoff.
    const cases = [
      [49.4, 'ok'],
      [49.5, 'warn'],
      [74.4, 'warn'],
      [74.5, 'orange'],
      [89.4, 'orange'],
      [89.5, 'bad'],
      [125, 'bad'],
    ] as const;

    for (const [pct, expected] of cases) expect(quotaTone(pct)).toBe(expected);
  });
  it('prefers statusline over oauth', () => {
    const s: Snapshot = { ...base,
      claude_statusline: { schema_version: 2, captured_at: '2026-06-03T12:00:00Z',
        payload: { rate_limits: { windows: [
          { key: 'five_hour', label: '5-hour', used_percent: 62, resets_at: '2026-06-03T14:41:00Z' },
          { key: 'seven_day', label: '7-day', used_percent: 48, resets_at: '2026-06-06T16:00:00Z' },
        ] }, session_cost_micro_usd: null, claude_code_version: null } },
      claude_oauth: { cadences: [{ key: 'five_hour', display_label: '5h', utilization_percent: 10, resets_at: '2026-06-03T14:41:00Z' }], extra_usage: null, subscription_type: null, rate_limit_tier: null, org_uuid: null, fetched_at: '2026-06-03T12:00:00Z' },
    };
    const q = anthropicQuota(s)!;
    expect(q.source).toBe('statusline');
    expect(q.headline.pct).toBe(62);
    expect(q.secondary?.pct).toBe(48);
  });
  it('falls back to oauth when the statusline payload is stale', () => {
    // Same data as above, but captured_at is >15min before fetched_at (a frozen
    // statusline file). The live OAuth source must win rather than the 62%
    // statusline reading being shown as current.
    const s: Snapshot = { ...base,
      fetched_at: '2026-06-03T12:00:00Z',
      claude_statusline: { schema_version: 2, captured_at: '2026-06-03T11:00:00Z', // 60min stale
        payload: { rate_limits: { windows: [
          { key: 'five_hour', label: '5-hour', used_percent: 62, resets_at: '2026-06-03T14:41:00Z' },
        ] }, session_cost_micro_usd: null, claude_code_version: null } },
      claude_oauth: { cadences: [{ key: 'five_hour', display_label: '5h', utilization_percent: 10, resets_at: '2026-06-03T14:41:00Z' }], extra_usage: null, subscription_type: null, rate_limit_tier: null, org_uuid: null, fetched_at: '2026-06-03T12:00:00Z' },
    };
    const q = anthropicQuota(s)!;
    expect(q.source).toBe('oauth');
    expect(q.headline.pct).toBe(10);
  });
  it('treats a future-dated statusline as stale and falls back to oauth', () => {
    // captured_at AFTER fetched_at (clock moved backward): negative age must
    // fail the freshness check, not slip through the upper bound as "fresh".
    const s: Snapshot = { ...base,
      fetched_at: '2026-06-03T12:00:00Z',
      claude_statusline: { schema_version: 2, captured_at: '2026-06-03T18:00:00Z', // 6h in the future
        payload: { rate_limits: { windows: [
          { key: 'five_hour', label: '5-hour', used_percent: 62, resets_at: '2026-06-03T14:41:00Z' },
        ] }, session_cost_micro_usd: null, claude_code_version: null } },
      claude_oauth: { cadences: [{ key: 'five_hour', display_label: '5h', utilization_percent: 10, resets_at: '2026-06-03T14:41:00Z' }], extra_usage: null, subscription_type: null, rate_limit_tier: null, org_uuid: null, fetched_at: '2026-06-03T12:00:00Z' },
    };
    const q = anthropicQuota(s)!;
    expect(q.source).toBe('oauth');
    expect(q.headline.pct).toBe(10);
  });
  it('returns null when statusline is stale and no oauth is present', () => {
    // Frozen statusline, OAuth absent (the 429 cold-start case): show nothing
    // live rather than a stale reading. The caller renders the stale/error state.
    const s: Snapshot = { ...base,
      fetched_at: '2026-06-03T12:00:00Z',
      claude_statusline: { schema_version: 2, captured_at: '2026-06-01T12:00:00Z', // 48h stale
        payload: { rate_limits: { windows: [
          { key: 'five_hour', label: '5-hour', used_percent: 62, resets_at: '2026-06-03T14:41:00Z' },
        ] }, session_cost_micro_usd: null, claude_code_version: null } },
    };
    expect(anthropicQuota(s)).toBeNull();
  });
  it('codex elapsed fraction', () => {
    const f = codexElapsedFraction({ resets_at: '2026-06-03T13:00:00Z', window_duration_minutes: 120 }, '2026-06-03T12:00:00Z');
    expect(f).toBeCloseTo(0.5, 5);
  });
  it('codex window expired when fetched_at is past resets_at', () => {
    const fetchedAt = '2026-06-03T12:00:00Z';
    // resets_at one hour before fetched_at -> the rollout outlived its window.
    expect(codexWindowExpired({ resets_at: '2026-06-03T11:00:00Z' }, fetchedAt)).toBe(true);
    // resets_at after fetched_at -> still live.
    expect(codexWindowExpired({ resets_at: '2026-06-03T13:00:00Z' }, fetchedAt)).toBe(false);
    // Unparseable timestamp must not be reported as expired (no false stale).
    expect(codexWindowExpired({ resets_at: 'not-a-date' }, fetchedAt)).toBe(false);
  });

  const codexSnap = (primary: { used_percent: number; window_duration_minutes: number; resets_at: string }, secondary: typeof primary | null, plan = 'pro'): Snapshot => ({
    ...base,
    codex_quota: { observed_at: '2026-07-08T10:00:00Z', session_id: 's', primary, secondary, plan_type: plan, rate_limit_reached: false },
  });

  it('codexQuota: 5h headline + weekly secondary on two-window plans', () => {
    const s = codexSnap(
      { used_percent: 1, window_duration_minutes: 300, resets_at: '2026-07-08T06:03:41Z' },
      { used_percent: 2, window_duration_minutes: 10080, resets_at: '2026-07-14T04:25:36Z' },
    );
    const q = codexQuota(s)!;
    expect(q.headline.label).toBe('5h');
    expect(q.headline.pct).toBe(1);
    expect(q.secondaryPct).toBe(2);
    expect(q.plan).toBe('pro');
  });

  it('codexQuota: single weekly window becomes the headline (go plan)', () => {
    const s = codexSnap({ used_percent: 3, window_duration_minutes: 10080, resets_at: '2026-07-14T04:25:36Z' }, null, 'go');
    const q = codexQuota(s)!;
    expect(q.headline.label).toBe('weekly');
    expect(q.headline.pct).toBe(3);
    expect(q.secondaryPct).toBeNull();
  });

  it('codexQuota: null snapshot -> null', () => {
    expect(codexQuota(base)).toBeNull();
  });
});

describe('extra-usage overage', () => {
  const eu = (over: Partial<ExtraUsage>): ExtraUsage => ({
    is_enabled: true,
    monthly_limit_micro_usd: 100_000_000,
    used_credits_micro_usd: 23_500_000,
    utilization_percent: 23.5,
    currency: 'USD',
    ...over,
  });

  it('classifyOverage: null / enabled / over-limit / disabled-with-no-spend', () => {
    expect(classifyOverage(null)).toBe('not-configured');
    expect(classifyOverage(eu({ is_enabled: true }))).toBe('active');
    // The bug: disabled past the cap, but used >= limit is REAL billed money.
    expect(
      classifyOverage(eu({ is_enabled: false, monthly_limit_micro_usd: 45_000_000, used_credits_micro_usd: 45_580_000 })),
    ).toBe('over-limit');
    // Disabled with no accrued spend (limit 0 / used 0) is genuinely not configured.
    expect(classifyOverage(eu({ is_enabled: false, monthly_limit_micro_usd: 0, used_credits_micro_usd: 0 }))).toBe('not-configured');
  });

  it('classifyOverage: breach is detected from used >= limit, NOT the clamped utilization', () => {
    // utilization clamped to 100 must not decide the state; used >= limit does.
    const over = eu({ is_enabled: false, monthly_limit_micro_usd: 45_000_000, used_credits_micro_usd: 45_580_000, utilization_percent: 100 });
    expect(classifyOverage(over)).toBe('over-limit');
    // used < limit while disabled is not an over-limit breach.
    const under = eu({ is_enabled: false, monthly_limit_micro_usd: 45_000_000, used_credits_micro_usd: 44_000_000, utilization_percent: 100 });
    expect(classifyOverage(under)).toBe('not-configured');
  });

  it('overageCell: over-limit shows the REAL billed amount, not the "none" placeholder', () => {
    const cell = overageCell(eu({ is_enabled: false, monthly_limit_micro_usd: 45_000_000, used_credits_micro_usd: 45_580_000 }));
    expect(cell.amount).toBe('$45.58/$45.00');
    expect(cell.badge).toBe('real');
    expect(cell.note).toContain('over limit');
    expect(cell.placeholder).toBeUndefined();
  });

  it('overageCell: active shows spend; not-configured shows the none placeholder', () => {
    const active = overageCell(eu({ is_enabled: true, monthly_limit_micro_usd: 100_000_000, used_credits_micro_usd: 23_500_000 }));
    expect(active.amount).toBe('$23.50/$100.00');
    expect(active.badge).toBe('real');

    const none = overageCell(null);
    expect(none.amount).toBeNull();
    expect(none.placeholder).toBe('none');
    expect(none.badge).toBeUndefined();
  });
});
