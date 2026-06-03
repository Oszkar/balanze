import { describe, it, expect } from 'vitest';
import { quotaTone, anthropicQuota, codexElapsedFraction } from './quota';
import type { Snapshot } from '../types/snapshot';

const base: Snapshot = {
  fetched_at: '2026-06-03T12:00:00Z',
  claude_oauth: null, claude_oauth_error: null,
  claude_jsonl: null, claude_jsonl_error: null,
  anthropic_api_cost: null, anthropic_api_cost_error: null,
  codex_quota: null, codex_quota_error: null,
  openai: null, openai_error: null,
  claude_statusline: null, claude_statusline_error: null,
  pace: [],
};

describe('quota', () => {
  it('tone buckets at 50/75/90', () => {
    expect(quotaTone(20)).toBe('ok');
    expect(quotaTone(60)).toBe('warn');
    expect(quotaTone(95)).toBe('bad');
    expect(quotaTone(49)).toBe('ok');
    expect(quotaTone(50)).toBe('warn');
    expect(quotaTone(89)).toBe('warn');
    expect(quotaTone(90)).toBe('bad');
  });
  it('prefers statusline over oauth', () => {
    const s: Snapshot = { ...base,
      claude_statusline: { schema_version: 1, captured_at: '2026-06-03T12:00:00Z',
        payload: { rate_limits: { five_hour: { used_percent: 62, resets_at: '2026-06-03T14:41:00Z' }, seven_day: { used_percent: 48, resets_at: '2026-06-06T16:00:00Z' } }, session_cost_micro_usd: null, claude_code_version: null } },
      claude_oauth: { cadences: [{ key: 'five_hour', display_label: '5h', utilization_percent: 10, resets_at: '2026-06-03T14:41:00Z' }], extra_usage: null, subscription_type: null, rate_limit_tier: null, org_uuid: null, fetched_at: '2026-06-03T12:00:00Z' },
    };
    const q = anthropicQuota(s)!;
    expect(q.source).toBe('statusline');
    expect(q.headline.pct).toBe(62);
    expect(q.secondary?.pct).toBe(48);
  });
  it('codex elapsed fraction', () => {
    const f = codexElapsedFraction({ resets_at: '2026-06-03T13:00:00Z', window_duration_minutes: 120 }, new Date('2026-06-03T12:00:00Z'));
    expect(f).toBeCloseTo(0.5, 5);
  });
});
