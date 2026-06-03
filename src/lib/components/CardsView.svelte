<script lang="ts">
  import type { Snapshot } from '$lib/types/snapshot';
  import { quotaTone, codexElapsedFraction } from '$lib/presentation/quota';
  import { microUsdToDollars } from '$lib/presentation/format';
  import ProviderCard, { type CardWindow } from './ProviderCard.svelte';

  let { snapshot }: { snapshot: Snapshot } = $props();

  // Anthropic windows: prefer OAuth cadences (one bar each, with the pace tick);
  // fall back to the live statusline 5h/7d when OAuth is absent — so Cards shows
  // the same Anthropic quota Grid does (which uses the statusline-preferred
  // helper) instead of dropping the provider card.
  const anthWindows = $derived.by<CardWindow[]>(() => {
    const cad = snapshot.claude_oauth?.cadences ?? [];
    if (cad.length > 0) {
      return cad.map((c) => {
        const pace = snapshot.pace.find((p) => p.key === c.key);
        return {
          label: c.display_label,
          used: c.utilization_percent,
          elapsed: pace ? pace.elapsed_fraction * 100 : null,
          tone: quotaTone(c.utilization_percent),
          resetsAt: c.resets_at,
        };
      });
    }
    const rl = snapshot.claude_statusline?.payload.rate_limits;
    const out: CardWindow[] = [];
    if (rl?.five_hour)
      out.push({ label: '5-hour', used: rl.five_hour.used_percent, elapsed: null, tone: quotaTone(rl.five_hour.used_percent), resetsAt: rl.five_hour.resets_at });
    if (rl?.seven_day)
      out.push({ label: '7-day', used: rl.seven_day.used_percent, elapsed: null, tone: quotaTone(rl.seven_day.used_percent), resetsAt: rl.seven_day.resets_at });
    return out;
  });
  const eu = $derived(snapshot.claude_oauth?.extra_usage ?? null);
  const codex = $derived(snapshot.codex_quota);
  const openai = $derived(snapshot.openai);
  const anthPlan = $derived(snapshot.claude_oauth?.subscription_type ?? 'Claude');
  // Match GridView: OpenAI is present if Codex quota, billed spend, or an error exists.
  const hasOpenAI = $derived(!!codex || !!openai || !!snapshot.openai_error);
</script>

<div class="cards">
  <ProviderCard name="Anthropic · Claude" plan={anthPlan}
    windows={anthWindows}
    billed={eu?.is_enabled
      ? { amount: `${microUsdToDollars(eu.used_credits_micro_usd)}/${microUsdToDollars(eu.monthly_limit_micro_usd)}`, note: 'overage', badge: 'real' }
      : { amount: null, note: 'API spend — unavailable', badge: 'na' }} />
  {#if hasOpenAI}
    <ProviderCard name="OpenAI" plan="API + Codex"
      windows={codex
        ? [{ label: `Codex · ${codex.plan_type}`, used: codex.primary.used_percent,
            elapsed: codexElapsedFraction(codex.primary) * 100, tone: quotaTone(codex.primary.used_percent), resetsAt: codex.primary.resets_at }]
        : []}
      billed={openai
        ? { amount: microUsdToDollars(Math.round(openai.total_usd * 1_000_000)), note: 'this cycle', badge: 'real' }
        : { amount: null, note: snapshot.openai_error ? '✗ fetch failed' : 'spend — unavailable', badge: 'na' }} />
  {/if}
</div>

<style>
  .cards { padding: 2px 16px 0; display: flex; flex-direction: column; gap: 10px; }
</style>
