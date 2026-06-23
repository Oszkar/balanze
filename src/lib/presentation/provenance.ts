export type BadgeKind = 'real' | 'quota' | 'est' | 'na';
export interface Provenance { badge: BadgeKind; title: string; }

export const PROV = {
  anthropicQuotaStatusline: { badge: 'quota', title: 'Server-reported quota · statusline (live) · 5-hour, the worst active window' },
  anthropicQuotaOauth:      { badge: 'quota', title: 'Server-reported quota · OAuth (fallback) · 5-hour, the worst active window' },
  codexQuota:               { badge: 'quota', title: 'Server-reported quota · Codex CLI rate-limit · 5-hour rolling' },
  anthropicBilledNa:        { badge: 'na',    title: 'No extra-usage overage this cycle · claude.ai pay-as-you-go only; Anthropic exposes no per-user API-spend endpoint - never estimated here' },
  anthropicBilledOverage:   { badge: 'real',  title: 'Real billed spend · claude.ai pay-as-you-go extra-usage overage · confidence: exact' },
  openaiBilled:             { badge: 'real',  title: 'Real billed spend · OpenAI Admin Costs API · this cycle · confidence: exact' },
  leverageEstimate:         { badge: 'est',   title: 'Estimate · local JSONL × API list prices · subscription leverage, never billed' },
} as const satisfies Record<string, Provenance>;
