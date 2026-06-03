import type { Snapshot } from '../types/snapshot';
import type { Tone } from './pace';

export function quotaTone(pct: number): Tone {
  if (pct >= 90) return 'bad';
  if (pct >= 75) return 'warn';
  if (pct >= 50) return 'warn';
  return 'ok';
}

export interface QuotaWindow { pct: number; resetsAt: string; label: string; }
export interface AnthropicQuota {
  headline: QuotaWindow;
  secondary: QuotaWindow | null;
  source: 'statusline' | 'oauth';
  tone: Tone;
}

export function anthropicQuota(s: Snapshot): AnthropicQuota | null {
  const sl = s.claude_statusline?.payload.rate_limits;
  if (sl?.five_hour) {
    const five = sl.five_hour;
    const seven = sl.seven_day;
    return {
      headline: { pct: five.used_percent, resetsAt: five.resets_at, label: '5h' },
      secondary: seven ? { pct: seven.used_percent, resetsAt: seven.resets_at, label: '7-day' } : null,
      source: 'statusline',
      tone: quotaTone(five.used_percent),
    };
  }
  const cad = s.claude_oauth?.cadences ?? [];
  const five = cad.find((c) => c.key === 'five_hour');
  if (!five) return null;
  const seven = cad.find((c) => c.key === 'seven_day');
  return {
    headline: { pct: five.utilization_percent, resetsAt: five.resets_at, label: '5h' },
    secondary: seven ? { pct: seven.utilization_percent, resetsAt: seven.resets_at, label: '7-day' } : null,
    source: 'oauth',
    tone: quotaTone(five.utilization_percent),
  };
}

export function codexElapsedFraction(w: { resets_at: string; window_duration_minutes: number }, now = new Date()): number {
  const totalMs = w.window_duration_minutes * 60_000;
  const remainMs = new Date(w.resets_at).getTime() - now.getTime();
  if (totalMs <= 0) return 0;
  const elapsed = 1 - remainMs / totalMs;
  return Math.min(1, Math.max(0, elapsed));
}
