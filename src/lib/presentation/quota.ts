import type { Snapshot, CodexQuotaSnapshot, RateLimitWindow } from '../types/snapshot';
import type { Tone } from './pace';

// Canonical quota-tone thresholds (percent utilization). These mirror the
// shared `window::Severity` classifier (crates/window/src/lib.rs) - the one
// green/yellow/orange/red heat scale at 50 / 75 / 90 used by the tray, CLI, and
// statusline. This popover cannot import the Rust crate, so it re-declares the
// cutoffs here; keep them in lockstep with SEVERITY_YELLOW/ORANGE/RED_PCT.
export const QUOTA_WARN_PCT = 50;
export const QUOTA_ORANGE_PCT = 75;
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
  // Classify the ROUNDED value so the tone matches the toFixed(0) label the
  // cells render: 74.6 shows "75%" and must read orange, not warn. Mirrors the
  // Rust surfaces, which classify their rounded display value too.
  const p = Math.round(pct);
  if (p >= QUOTA_BAD_PCT) return 'bad';
  if (p >= QUOTA_ORANGE_PCT) return 'orange';
  if (p >= QUOTA_WARN_PCT) return 'warn';
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

export interface CodexQuota {
  headline: { pct: number; resetsAt: string; window: RateLimitWindow; label: '5h' | 'weekly' | 'codex' };
  secondaryPct: number | null;
  plan: string;
  tone: Tone;
}

// Codex reports windows of 300 min (5h) and 10080 min (weekly); which JSON slot
// holds which varies by plan, so select by duration, never by position.
export function codexWindowsByKind(q: CodexQuotaSnapshot): { five: RateLimitWindow | null; weekly: RateLimitWindow | null } {
  const windows = [q.primary, ...(q.secondary ? [q.secondary] : [])];
  return {
    five: windows.find((w) => w.window_duration_minutes === 300) ?? null,
    weekly: windows.find((w) => w.window_duration_minutes === 10080) ?? null,
  };
}

export function codexQuota(s: Snapshot): CodexQuota | null {
  const q = s.codex_quota;
  if (!q) return null;
  const { five, weekly } = codexWindowsByKind(q);
  const headlineWin = five ?? weekly ?? q.primary;
  const label = headlineWin === five ? '5h' : headlineWin === weekly ? 'weekly' : 'codex';
  return {
    headline: { pct: headlineWin.used_percent, resetsAt: headlineWin.resets_at, window: headlineWin, label },
    secondaryPct: five && weekly ? weekly.used_percent : null,
    plan: q.plan_type,
    tone: quotaTone(headlineWin.used_percent),
  };
}
