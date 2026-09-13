// USD display formatter (display boundary only - see AGENTS.md currency rule).
// Intl.NumberFormat over a hand-rolled `$` + toFixed gives thousands separators
// and correct symbol/negative handling. Locale pinned to en-US so output stays
// deterministic; the app's amounts are provider-billed in USD.
const USD = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' });

const MICRO_PER_CENT = 10_000;

// Round micro-USD to whole cents, half away from zero. This is the shared
// policy the Rust `money` crate implements; the `money` vectors in
// tests/fixtures/presentation-policy.json pin both languages to it.
//
// Rounding the integer BEFORE formatting is the point. Handing Intl the raw
// quotient made the two languages disagree at a half cent: 1_005_000 micro-USD
// is 1.005, which Intl rounds up from the shortest decimal ($1.01) while Rust's
// `{:.2}` rounds the exact binary double 1.00499999... down ($1.00).
export const microUsdToCents = (micro: number): number => {
  const cents = Math.floor((Math.abs(micro) + MICRO_PER_CENT / 2) / MICRO_PER_CENT);
  // The `cents !== 0` guard is JavaScript-only: negating a zero here yields
  // -0, which Intl renders as "-$0.00". Rust's integers have no such value,
  // so this is what keeps the two implementations equal at the boundary.
  return micro < 0 && cents !== 0 ? -cents : cents;
};

// Intl still owns the symbol, the thousands separators, and the sign placement
// - only the cent value is contractual across languages. It never sees a
// fraction of a cent, so its own rounding mode cannot come into play.
export const microUsdToDollars = (micro: number): string => USD.format(microUsdToCents(micro) / 100);

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
