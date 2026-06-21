// USD display formatter (display boundary only - see AGENTS.md currency rule).
// Intl.NumberFormat over a hand-rolled `$` + toFixed gives thousands separators
// and correct symbol/negative handling. Locale pinned to en-US so output stays
// deterministic; the app's amounts are provider-billed in USD.
const USD = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' });
export const microUsdToDollars = (micro: number): string => USD.format(micro / 1_000_000);

export function relativeReset(isoResetsAt: string, now: Date = new Date()): string {
  const ms = new Date(isoResetsAt).getTime() - now.getTime();
  if (ms <= 0) return '(passed)';
  const mins = Math.floor(ms / 60000);
  const d = Math.floor(mins / 1440), h = Math.floor((mins % 1440) / 60), m = mins % 60;
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

export function formatBurn(tokensPerMin: number | null): string {
  if (tokensPerMin == null) return '-';
  if (tokensPerMin >= 1000) return `~${(tokensPerMin / 1000).toFixed(1)}k/min`;
  return `~${Math.round(tokensPerMin)}/min`;
}
