import { describe, it, expect } from 'vitest';
import { quotaTone, anthropicQuota, codexElapsedFraction, codexWindowExpired } from './quota';
import type { Snapshot } from '../types/snapshot';

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
  it('tone folds to warn at 50 and bad at 90 (the tray orange band at 75 folds into warn)', () => {
    expect(quotaTone(20)).toBe('ok');
    expect(quotaTone(49)).toBe('ok');
    expect(quotaTone(50)).toBe('warn');
    expect(quotaTone(60)).toBe('warn');
    // 75 is the tray's yellow->orange boundary; this 3-tone palette keeps it 'warn'.
    expect(quotaTone(75)).toBe('warn');
    expect(quotaTone(89)).toBe('warn');
    expect(quotaTone(90)).toBe('bad');
    expect(quotaTone(95)).toBe('bad');
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
});
