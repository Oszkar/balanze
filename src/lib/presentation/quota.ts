import type { Snapshot } from '../types/snapshot';
import type { Tone } from './pace';

// Canonical quota-tone thresholds (percent utilization), mirroring the WARN and
// BAD boundaries in src-tauri/src/tauri_sink.rs (ColorBucket::from_util). The
// tray uses a 4-color palette with an extra ORANGE band at 75%; this popover
// uses a coarser 3-tone palette and folds 75-90% into 'warn'. Keep the shared
// boundary values (50, 90) in lockstep across the two files.
export const QUOTA_WARN_PCT = 50;
export const QUOTA_BAD_PCT = 90;

export function quotaTone(pct: number): Tone {
  if (pct >= QUOTA_BAD_PCT) return 'bad';
  // 50-90 is one 'warn' tone here; the tray splits it into yellow (50-75) and
  // orange (75-90), which this 3-tone palette intentionally folds together.
  if (pct >= QUOTA_WARN_PCT) return 'warn';
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
  const slWindows = s.claude_statusline?.payload.rate_limits?.windows ?? [];
  const slFive = slWindows.find((w) => w.key === 'five_hour');
  if (slFive) {
    const slSeven = slWindows.find((w) => w.key === 'seven_day');
    return {
      headline: { pct: slFive.used_percent, resetsAt: slFive.resets_at, label: '5h' },
      secondary: slSeven ? { pct: slSeven.used_percent, resetsAt: slSeven.resets_at, label: '7-day' } : null,
      source: 'statusline',
      tone: quotaTone(slFive.used_percent),
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

// A Codex rollout whose primary window has already reset is stale: the
// used_percent it carries describes an elapsed window, so the cell should be
// flagged rather than shown as a confident figure. Evaluated against the
// snapshot's `fetched_at` (NOT wall-clock now) so it matches the CLI's rule in
// crates/balanze_cli/src/render.rs (`compact_codex_quota`: fetched_at > resets_at)
// and stays consistent with the snapshot's other time-relative math. Unparseable
// timestamps return false (NaN comparisons are false) so we never falsely mark stale.
export function codexWindowExpired(w: { resets_at: string }, fetchedAt: string): boolean {
  return new Date(w.resets_at).getTime() < new Date(fetchedAt).getTime();
}

// Codex window elapsed fraction, relative to the snapshot's `fetched_at` (not
// wall-clock now) so it stays consistent with `codexWindowExpired` and the
// snapshot-anchored pace math. No CLI counterpart (the CLI shows no codex pace
// bar). Clamped to [0, 1].
export function codexElapsedFraction(
  w: { resets_at: string; window_duration_minutes: number },
  fetchedAt: string,
): number {
  const totalMs = w.window_duration_minutes * 60_000;
  const remainMs = new Date(w.resets_at).getTime() - new Date(fetchedAt).getTime();
  if (totalMs <= 0) return 0;
  const elapsed = 1 - remainMs / totalMs;
  return Math.min(1, Math.max(0, elapsed));
}
