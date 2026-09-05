import type { Snapshot, CodexQuotaSnapshot, RateLimitWindow, ExtraUsage, WindowPace } from '../types/snapshot';
import type { Tone } from './pace';
import { microUsdToDollars } from './format';
import { PROV } from './provenance';

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

export interface QuotaWindow { key: string; pct: number; resetsAt: string; label: string; }
export interface AnthropicQuota {
  headline: QuotaWindow;
  secondary: QuotaWindow | null;
  source: 'statusline' | 'oauth';
  tone: Tone;
}

export type AnthropicSourceWindow = QuotaWindow;
export interface AnthropicSourceView {
  source: 'statusline' | 'oauth';
  windows: AnthropicSourceWindow[];
  stale: boolean;
}

/** Fresh statusline-first Anthropic quota selection, mirroring
 * `Snapshot::anthropic_quota_source` on the Rust side. */
export function anthropicSourceView(s: Snapshot): AnthropicSourceView | null {
  const sl = s.claude_statusline;
  const slAgeMs = sl ? Date.parse(s.fetched_at) - Date.parse(sl.captured_at) : Infinity;
  const timestampError = s.claude_statusline_error?.startsWith('statusline payload is stale (')
    || s.claude_statusline_error?.startsWith('statusline payload is future-dated (');
  const slFresh = !timestampError && Number.isFinite(slAgeMs) && slAgeMs >= 0 && slAgeMs <= STATUSLINE_FRESHNESS_MS;
  const slWindows = slFresh && sl ? (sl.payload.rate_limits?.windows ?? []) : [];
  if (slWindows.length > 0) {
    return {
      source: 'statusline',
      windows: slWindows.map((w) => ({ key: w.key, label: w.label, pct: w.used_percent, resetsAt: w.resets_at })),
      stale: s.claude_statusline_error !== null,
    };
  }
  const cadences = s.claude_oauth?.cadences ?? [];
  if (cadences.length === 0) return null;
  return {
    source: 'oauth',
    windows: cadences.map((c) => ({ key: c.key, label: c.display_label, pct: c.utilization_percent, resetsAt: c.resets_at })),
    stale: s.claude_oauth_error !== null,
  };
}

/** Pace entries whose keys belong to the selected Anthropic quota source. */
export function matchingAnthropicPace(s: Snapshot): WindowPace[] {
  const selected = anthropicSourceView(s);
  if (!selected) return [];
  const selectedKeys = new Set(selected.windows.map((w) => w.key));
  return s.pace.filter((pace) => selectedKeys.has(pace.key));
}

export function anthropicQuota(s: Snapshot): AnthropicQuota | null {
  const selected = anthropicSourceView(s);
  if (!selected) return null;
  const five = selected.windows.find((w) => w.key === 'five_hour') ?? null;
  const seven = selected.windows
    .filter((w) => w.key !== 'five_hour')
    .reduce<AnthropicSourceWindow | null>((worst, w) => !worst || w.pct > worst.pct ? w : worst, null);
  const headline = five ?? seven;
  if (!headline) return null;
  return {
    headline: { key: headline.key, pct: headline.pct, resetsAt: headline.resetsAt, label: five ? '5h' : '7-day' },
    secondary: five && seven ? { key: seven.key, pct: seven.pct, resetsAt: seven.resetsAt, label: '7-day' } : null,
    source: selected.source,
    tone: quotaTone(Math.max(five?.pct ?? 0, seven?.pct ?? 0)),
  };
}

// Lockstep mirror of codex_local::RateLimitWindow::expired, pinned by
// tests/fixtures/presentation-policy.json: strict fetched_at > resets_at.
// A Codex rollout with any window that has already reset is stale: the
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

// Display label for a Codex window, by duration. `window` is the honest label
// for a duration Codex has not reported before (a taxonomy change) - such a
// window is still selectable rather than dropped, mirroring the watch TUI's
// "never silently drop a live cap" rule.
export type CodexWindowLabel = '5h' | '7d' | 'window';
// Keep this mirror in lockstep with codex_local::WINDOW_DURATION_TOLERANCE_MINUTES.
export const CODEX_WINDOW_TOLERANCE_MINUTES = 5;

function nearDuration(actual: number, expected: number): boolean {
  return Math.abs(actual - expected) <= CODEX_WINDOW_TOLERANCE_MINUTES;
}

export function codexWindowLabel(w: RateLimitWindow): CodexWindowLabel {
  if (nearDuration(w.window_duration_minutes, 300)) return '5h';
  if (nearDuration(w.window_duration_minutes, 10080)) return '7d';
  return 'window';
}

export interface CodexQuota {
  headline: { pct: number; resetsAt: string; window: RateLimitWindow; label: CodexWindowLabel };
  /// The other window, demoted to the cell's secondary text. `null` on
  /// single-window plans (e.g. "go").
  secondary: { pct: number; label: CodexWindowLabel } | null;
  plan: string;
  tone: Tone;
  /// True when ANY window has reset - not just the headline. The cell carries
  /// one stale marker for the whole rollout, so checking only the headline let
  /// a live-but-worse window hide an expired one (a reset 5h window demoted to
  /// secondary text under a live weekly headline). `any`, not `all`, mirrors
  /// `codex_local::CodexQuotaSnapshot::any_window_expired`: once the shortest
  /// window has reset the rollout predates it, so the still-live window's
  /// figure is an undercount too.
  expired: boolean;
}

// Codex reports windows of 300 min (5h) and 10080 min (weekly); which JSON slot
// holds which varies by plan, so select by duration, never by position.
// `weekly` names the second presentation slot. When Codex introduces an
// unknown cadence, that slot deliberately carries the worst unclassified cap.
export function codexWindowsByKind(q: CodexQuotaSnapshot): { five: RateLimitWindow | null; weekly: RateLimitWindow | null } {
  const windows = [q.primary, ...(q.secondary ? [q.secondary] : [])];
  const five = windows.find((w) => nearDuration(w.window_duration_minutes, 300)) ?? null;
  const weekly = windows
    .filter((w) => w !== five)
    .reduce<RateLimitWindow | null>((worst, w) => !worst || w.used_percent > worst.used_percent ? w : worst, null);
  return {
    five,
    weekly,
  };
}

// The grid tile shows ONE Codex window, so it must be the worst
// (highest-utilization) one - "how close to a limit am I". This mirrors
// `codex_local::worst_window` (whose doc names this very cell), which also
// supplies the tray and CLI headlines. Keying on the 5h slot
// instead let a weekly window at 95% sit as grey secondary text under a green
// 4% tile while the tray painted red - the two surfaces disagreed on the same
// snapshot. Reduces over EVERY window rather than the known 300/10080 durations
// so an unrecognized duration cannot hide a live cap.
// Keep selection and any-window expiry in lockstep with the Rust contract,
// pinned by tests/fixtures/presentation-policy.json: compare raw percentages,
// keep primary on ties, and retain expired windows with a separate stale flag.
export function codexQuota(s: Snapshot): CodexQuota | null {
  const q = s.codex_quota;
  if (!q) return null;
  const all: RateLimitWindow[] = [q.primary, ...(q.secondary ? [q.secondary] : [])];
  const headlineWin = all.reduce((a, b) => (b.used_percent > a.used_percent ? b : a));
  const secondaryWin = all.find((w) => w !== headlineWin) ?? null;
  return {
    headline: {
      pct: headlineWin.used_percent,
      resetsAt: headlineWin.resets_at,
      window: headlineWin,
      label: codexWindowLabel(headlineWin),
    },
    secondary: secondaryWin
      ? { pct: secondaryWin.used_percent, label: codexWindowLabel(secondaryWin) }
      : null,
    plan: q.plan_type,
    tone: quotaTone(headlineWin.used_percent),
    expired: all.some((w) => codexWindowExpired(w, s.fetched_at)),
  };
}

// Three-state classification of the claude.ai pay-as-you-go extra-usage
// overage, mirroring the CLI's `classify_overage`
// (crates/balanze_cli/src/render.rs) so the popover and CLI cannot diverge.
// GOTCHA: once usage exceeds the monthly cap Anthropic flips `is_enabled` to
// false but KEEPS the real billed used/limit and clamps `utilization_percent`
// to 100.0, so a naive `is_enabled` gate hides real billed money at its peak.
// Detect the breach from `used_credits >= monthly_limit`, NEVER from
// `utilization_percent`.
export type OverageState = 'active' | 'over-limit' | 'not-configured';

export function classifyOverage(eu: ExtraUsage | null): OverageState {
  if (!eu) return 'not-configured';
  if (eu.is_enabled) return 'active';
  if (eu.monthly_limit_micro_usd > 0 && eu.used_credits_micro_usd >= eu.monthly_limit_micro_usd) {
    return 'over-limit';
  }
  return 'not-configured';
}

// The billed-cell descriptor for the Anthropic extra-usage overage, shared by
// GridView and CardsView so the two surfaces (and the CLI) stay in lockstep -
// the divergence between them was the bug this fixes. `active` and `over-limit`
// both surface the real billed `$used/$limit` with the `real` badge; only the
// note differs (over-limit signals the cap is reached). `not-configured` keeps
// the neutral "none" placeholder.
export interface OverageCell {
  amount: string | null;
  placeholder?: 'none';
  note: string;
  badge?: 'real' | 'na';
  title: string;
}

export function overageCell(eu: ExtraUsage | null): OverageCell {
  const state = classifyOverage(eu);
  if (eu && (state === 'active' || state === 'over-limit')) {
    const amount = `${microUsdToDollars(eu.used_credits_micro_usd)}/${microUsdToDollars(eu.monthly_limit_micro_usd)}`;
    const note = state === 'over-limit' ? 'over limit · this cycle' : 'overage · this cycle';
    return { amount, note, badge: PROV.anthropicBilledOverage.badge, title: PROV.anthropicBilledOverage.title };
  }
  return { amount: null, placeholder: 'none', note: 'overage · this cycle', title: PROV.anthropicBilledNa.title };
}

export function openAiCostCell(s: Snapshot): { amount: string; note: string; title: string } | null {
  if (!s.openai) return null;
  const partial = s.openai.truncated;
  return {
    amount: `${microUsdToDollars(s.openai.total_micro_usd)}${partial ? '*' : ''}`,
    note: `${partial ? 'partial · ' : ''}admin api · this cycle`,
    title: partial ? `${PROV.openaiBilled.title} Partial total; more pages are available.` : PROV.openaiBilled.title,
  };
}
