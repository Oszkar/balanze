export const microUsdToDollars = (micro: number): string => `$${(micro / 1_000_000).toFixed(2)}`;

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
