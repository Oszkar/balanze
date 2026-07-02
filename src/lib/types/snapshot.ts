// Mirrors the raw serde shape emitted over IPC (NOT the CLI json_output DTO).
// i64/u64/f32/f64 -> number; Option<T> -> T | null; DateTime<Utc> -> ISO string.

export interface CadenceBar {
  key: string;                 // "five_hour" | "seven_day" | "seven_day_sonnet" | ...
  display_label: string;
  utilization_percent: number; // 0..100+
  resets_at: string;
}

export interface ExtraUsage {
  is_enabled: boolean;
  monthly_limit_micro_usd: number;
  used_credits_micro_usd: number;
  utilization_percent: number;
  currency: string;
}

export interface ClaudeOAuthSnapshot {
  cadences: CadenceBar[];
  extra_usage: ExtraUsage | null;
  subscription_type: string | null;
  rate_limit_tier: string | null;
  org_uuid: string | null;
  fetched_at: string;
}

export interface ByModel { model: string; events: number; total_tokens: number; }

// JsonlSnapshot flattens WindowSummary via #[serde(flatten)].
export interface JsonlSnapshot {
  files_scanned: number;
  window_start: string;
  total_events_in_window: number;
  total_tokens_in_window: number;
  recent_burn_tokens_per_min: number | null;
  by_model: ByModel[];
}

export interface ModelCost {
  model: string;
  event_count: number;
  input_micro_usd: number;
  output_micro_usd: number;
  cache_creation_micro_usd: number;
  cache_read_micro_usd: number;
  total_micro_usd: number;
}
export interface Cost {
  per_model: ModelCost[];
  total_micro_usd: number;
  skipped_models: string[];
  total_event_count: number;
  unparsed_event_count: number;
}

export interface RateLimitWindow {
  used_percent: number;
  window_duration_minutes: number;
  resets_at: string;
}
export interface CodexQuotaSnapshot {
  observed_at: string;
  session_id: string;
  primary: RateLimitWindow;
  secondary: RateLimitWindow | null;
  plan_type: string;
  rate_limit_reached: boolean;
}

export interface LineItemCost { line_item: string; amount_micro_usd: number; }
export interface OpenAiCosts {
  start_time: string;
  end_time: string;
  total_micro_usd: number;
  by_line_item: LineItemCost[];
  truncated: boolean;
  fetched_at: string;
}

export interface RateWindow { key: string; label: string; used_percent: number; resets_at: string; }
export interface RateLimits { windows: RateWindow[]; }
export interface StatuslineSnapshot {
  rate_limits: RateLimits | null;
  session_cost_micro_usd: number | null;
  claude_code_version: string | null;
}
export interface StatuslineFilePayload {
  schema_version: number;
  captured_at: string;
  payload: StatuslineSnapshot;
}

export interface WindowPace {
  key: string;
  used_fraction: number;
  elapsed_fraction: number;
  ratio: number | null;
}

export interface Snapshot {
  schema_version: number;
  fetched_at: string;
  claude_oauth: ClaudeOAuthSnapshot | null;
  claude_oauth_error: string | null;
  /** Neutral "not configured" marker (e.g. Claude Code not installed), distinct
   *  from claude_oauth_error. Mutually exclusive with claude_oauth data. */
  claude_oauth_unavailable: string | null;
  claude_jsonl: JsonlSnapshot | null;
  claude_jsonl_error: string | null;
  anthropic_api_cost: Cost | null;
  anthropic_api_cost_error: string | null;
  codex_quota: CodexQuotaSnapshot | null;
  codex_quota_error: string | null;
  openai: OpenAiCosts | null;
  openai_error: string | null;
  claude_statusline: StatuslineFilePayload | null;
  claude_statusline_error: string | null;
  pace: WindowPace[];
}

// Emitted by TauriSink::on_degraded.
export interface DegradedPayload { source: string; error: string; }
