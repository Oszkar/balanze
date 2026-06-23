// Hand-built fixtures for the dev-only states gallery (`/gallery`). Each entry is
// a real `Snapshot` shaped to drive one popover state through the real components
// and presentation logic - no IPC, no live data. See the "States gallery" note
// in README.md.

import type { Snapshot } from '$lib/types/snapshot';
import type { Settings, StatuslineWire } from '$lib/types/settings';

// Timestamps are anchored at module load so `relativeReset` (wall-clock relative)
// reads naturally ("2h left"). `iso(0)` is "now"; positive = future reset,
// negative = past (used to force a stale Codex window).
const NOW = Date.now();
const H = 3_600_000;
const iso = (offsetMs: number) => new Date(NOW + offsetMs).toISOString();

/** A fully-populated two-provider snapshot. Every state clones this and overrides. */
export function baseSnapshot(): Snapshot {
  return {
    schema_version: 2,
    fetched_at: iso(0),
    claude_oauth: {
      cadences: [
        { key: 'five_hour', display_label: '5-hour', utilization_percent: 62, resets_at: iso(2 * H) },
        { key: 'seven_day', display_label: '7-day', utilization_percent: 41, resets_at: iso(72 * H) },
      ],
      extra_usage: null,
      subscription_type: 'Max 20x',
      rate_limit_tier: null,
      org_uuid: null,
      fetched_at: iso(0),
    },
    claude_oauth_error: null,
    claude_oauth_unavailable: null,
    claude_jsonl: {
      files_scanned: 12,
      window_start: iso(-5 * H),
      total_events_in_window: 340,
      total_tokens_in_window: 1_280_000,
      recent_burn_tokens_per_min: 8200,
      by_model: [],
    },
    claude_jsonl_error: null,
    anthropic_api_cost: {
      per_model: [],
      total_micro_usd: 47_300_000, // ~$47.30 of subscription leverage
      skipped_models: [],
      total_event_count: 340,
      unparsed_event_count: 0,
    },
    anthropic_api_cost_error: null,
    codex_quota: {
      observed_at: iso(0),
      session_id: 'sess_demo',
      primary: { used_percent: 73, window_duration_minutes: 300, resets_at: iso(90 * 60_000) },
      secondary: null,
      plan_type: 'Plus',
      rate_limit_reached: false,
    },
    codex_quota_error: null,
    openai: {
      start_time: iso(-720 * H),
      end_time: iso(0),
      total_micro_usd: 12_840_000, // ~$12.84
      by_line_item: [],
      truncated: false,
      fetched_at: iso(0),
    },
    openai_error: null,
    claude_statusline: null,
    claude_statusline_error: null,
    pace: [
      { key: 'five_hour', used_fraction: 0.62, elapsed_fraction: 0.5, ratio: 1.24 },
      { key: 'seven_day', used_fraction: 0.41, elapsed_fraction: 0.6, ratio: 0.68 },
    ],
  };
}

// Small clone helper so each override starts from a fresh deep-ish copy (the
// nested objects we mutate are replaced wholesale below, so a structured clone
// keeps overrides from leaking across fixtures).
const clone = (s: Snapshot): Snapshot => structuredClone(s);

/** Anthropic quota not yet fetched (no oauth, no statusline, no error) -> loading. */
function coldStart(): Snapshot {
  const s = clone(baseSnapshot());
  s.claude_oauth = null;
  s.claude_oauth_error = null;
  s.claude_statusline = null;
  s.claude_statusline_error = null;
  s.pace = [];
  return s; // OpenAI side keeps Codex data so only the Anthropic cell loads.
}

/** Claude Code not installed: OAuth reports unavailable (neutral, not an error),
 *  no Codex/OpenAI -> the terminal "Claude Code not detected" cell, single
 *  column, and the Add OpenAI affordance. */
function claudeNotDetected(): Snapshot {
  const s = clone(baseSnapshot());
  s.claude_oauth = null;
  s.claude_oauth_error = null;
  s.claude_oauth_unavailable = 'Claude Code not detected';
  s.claude_jsonl = null;
  s.anthropic_api_cost = null;
  s.codex_quota = null;
  s.openai = null;
  s.openai_error = null;
  s.pace = [];
  return s;
}

/** Billing opted in but nothing to show and no error -> the connect CTA. */
function openaiConnect(): Snapshot {
  const s = clone(baseSnapshot());
  s.codex_quota = null;
  s.openai = null;
  s.openai_error = null;
  return s;
}

/** Billing opted in, the fetch failed -> the error placeholder spanning the column. */
function openaiError(): Snapshot {
  const s = clone(baseSnapshot());
  s.codex_quota = null;
  s.openai = null;
  s.openai_error = '401 - invalid admin key';
  return s;
}

/** Anthropic only: no Codex, no OpenAI spend -> the OpenAI column collapses. */
function singleProvider(): Snapshot {
  const s = clone(baseSnapshot());
  s.codex_quota = null;
  s.openai = null;
  s.openai_error = null;
  return s;
}

/** Codex primary window already reset (resets_at in the past) -> stale flag. */
function codexStale(): Snapshot {
  const s = clone(baseSnapshot());
  if (s.codex_quota) s.codex_quota.primary.resets_at = iso(-1 * H);
  return s;
}

/** OAuth quota with a stale statusline degrade -> the "fallback" stale label. */
function anthFallback(): Snapshot {
  return clone(baseSnapshot()); // degrade is supplied via the frame's `degraded` map
}

/** Per-user API overage enabled -> the Anthropic billed cell shows spend. */
function overageBilled(): Snapshot {
  const s = clone(baseSnapshot());
  if (s.claude_oauth) {
    s.claude_oauth.extra_usage = {
      is_enabled: true,
      monthly_limit_micro_usd: 100_000_000, // $100 cap
      used_credits_micro_usd: 23_500_000, // $23.50 used
      utilization_percent: 23.5,
      currency: 'USD',
    };
  }
  return s;
}

export interface GalleryState {
  label: string;
  view: 'grid' | 'cards' | 'settings' | 'empty';
  openaiEnabled?: boolean;
  snapshot?: Snapshot;
  degraded?: Record<string, string>;
  empty?: {
    title: string;
    body?: string;
    detail?: string | null;
    actions?: { label: string; kind?: 'primary' | 'secondary' }[];
  };
}

export const GALLERY_STATES: GalleryState[] = [
  { label: 'Grid - two providers', view: 'grid', openaiEnabled: true, snapshot: baseSnapshot() },
  { label: 'Grid - cold start (quota loading)', view: 'grid', openaiEnabled: true, snapshot: coldStart() },
  { label: 'Grid - Claude Code not detected', view: 'grid', openaiEnabled: false, snapshot: claudeNotDetected() },
  { label: 'Grid - OpenAI connect CTA', view: 'grid', openaiEnabled: true, snapshot: openaiConnect() },
  { label: 'Grid - OpenAI error', view: 'grid', openaiEnabled: true, snapshot: openaiError() },
  { label: 'Grid - Anthropic only', view: 'grid', openaiEnabled: false, snapshot: singleProvider() },
  { label: 'Grid - Codex stale window', view: 'grid', openaiEnabled: true, snapshot: codexStale() },
  {
    label: 'Grid - Anthropic statusline fallback',
    view: 'grid',
    openaiEnabled: true,
    snapshot: anthFallback(),
    degraded: { claude_statusline: 'statusline payload is stale' },
  },
  { label: 'Grid - overage billed', view: 'grid', openaiEnabled: true, snapshot: overageBilled() },

  { label: 'Cards - two providers', view: 'cards', openaiEnabled: true, snapshot: baseSnapshot() },
  { label: 'Cards - Anthropic only', view: 'cards', openaiEnabled: false, snapshot: singleProvider() },
  { label: 'Cards - Codex stale window', view: 'cards', openaiEnabled: true, snapshot: codexStale() },
  { label: 'Cards - OpenAI error', view: 'cards', openaiEnabled: true, snapshot: openaiError() },
  { label: 'Cards - cold start (quota loading)', view: 'cards', openaiEnabled: true, snapshot: coldStart() },
  { label: 'Cards - Claude Code not detected', view: 'cards', openaiEnabled: false, snapshot: claudeNotDetected() },
  {
    label: 'Cards - Anthropic statusline fallback',
    view: 'cards',
    openaiEnabled: true,
    snapshot: anthFallback(),
    degraded: { claude_statusline: 'statusline payload is stale' },
  },

  { label: 'Settings - configured', view: 'settings' },

  { label: 'Empty - starting up (no snapshot yet)', view: 'empty', empty: {
      title: 'Starting up...',
      body: 'Balanze is reading your local usage. This only takes a moment.',
      actions: [{ label: 'Retry', kind: 'primary' }],
  } },
  { label: 'Empty - backend not responding', view: 'empty', empty: {
      title: "Balanze isn't responding yet",
      body: 'The background service may still be starting.',
      detail: 'get_snapshot failed: connection refused',
      actions: [{ label: 'Retry', kind: 'primary' }],
  } },
];

// Canned IPC returns for the one Settings frame (see the route's mockIPC setup).
export const DEMO_SETTINGS: Settings = {
  version: 1,
  providers: { openai_enabled: true, anthropic_enabled: true, codex_enabled: true },
  oauth_poll_interval_secs: 300,
  seen_welcome: true,
};

export const DEMO_STATUSLINE: StatuslineWire = { status: 'unwired', command: null };
