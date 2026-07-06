import type { Snapshot } from '../types/snapshot';
import type { Tone } from './pace';

// Canonical quota-tone thresholds (percent utilization), mirroring the WARN and
// BAD boundaries in src-tauri/src/tauri_sink.rs (ColorBucket::from_util). The
// tray uses a 4-color palette with an extra ORANGE band at 75%; this popover
// uses a coarser 3-tone palette and folds 75-90% into 'warn'. Keep the shared
// boundary values (50, 90) in lockstep across the two files.
export const QUOTA_WARN_PCT = 50;
export const QUOTA_BAD_PCT = 90;

// Staleness ceiling for the statusLine payload, in milliseconds. Kept in
// lockstep with STATUSLINE_FRESHNESS_SECS (900) in
// crates/state_coordinator/src/snapshot.rs. When the payload's captured_at is
// older than this relative to the snapshot's fetched_at, the statusLine file has
// frozen (e.g. another tool owns the single statusLine slot so Balanze's writer
// never refreshes it) and must not be presented as the live Anthropic source.
// This is the render-time guard, independent of the coordinator's error slot
// (belt-and-suspenders): even if the backend marker regressed, the popover still
// refuses to show a stale statusline as live and falls back to OAuth.
export const STATUSLINE_FRESHNESS_MS = 900_000;

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
  // Only trust the statusLine as the live source while it is fresh. Age is
  // fetched_at - captured_at (NOT wall-clock now) so this stays a pure function
  // of the snapshot and matches codexWindowExpired's fetched_at anchor;
  // fetched_at is re-stamped on every coordinator emit, so it tracks "now"
  // within the safety-poll cadence. Fresh iff the age is within [0, threshold]:
  // unparseable timestamps -> NaN and a future-dated captured_at -> negative age
  // both fail the check, so we fall back to the live OAuth source rather than
  // trust a bad or clock-skewed stamp (mirrors the coordinator's ingest guard).
  const sl = s.claude_statusline;
  const slAgeMs = sl ? Date.parse(s.fetched_at) - Date.parse(sl.captured_at) : Infinity;
  const slFresh = Number.isFinite(slAgeMs) && slAgeMs >= 0 && slAgeMs <= STATUSLINE_FRESHNESS_MS;
  const slWindows = slFresh && sl ? (sl.payload.rate_limits?.windows ?? []) : [];
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
