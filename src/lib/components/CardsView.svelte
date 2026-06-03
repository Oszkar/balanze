<script lang="ts">
  import type { Snapshot } from '$lib/types/snapshot';
  import { quotaTone, codexElapsedFraction } from '$lib/presentation/quota';
  import { microUsdToDollars } from '$lib/presentation/format';
  import ProviderCard, { type CardWindow } from './ProviderCard.svelte';

  let { snapshot }: { snapshot: Snapshot } = $props();

  const anthWindows = $derived<CardWindow[]>(
    (snapshot.claude_oauth?.cadences ?? []).map((c) => {
      const pace = snapshot.pace.find((p) => p.key === c.key);
      return {
        label: c.display_label,
        used: c.utilization_percent,
        elapsed: pace ? pace.elapsed_fraction * 100 : null,
        tone: quotaTone(c.utilization_percent),
        resetsAt: c.resets_at,
      };
    })
  );
  const eu = $derived(snapshot.claude_oauth?.extra_usage ?? null);
  const codex = $derived(snapshot.codex_quota);
  const openai = $derived(snapshot.openai);
</script>

<div class="cards">
  {#if snapshot.claude_oauth}
    <ProviderCard name="Anthropic · Claude" plan={snapshot.claude_oauth.subscription_type ?? '—'}
      windows={anthWindows}
      billed={eu?.is_enabled
        ? { amount: `${microUsdToDollars(eu.used_credits_micro_usd)}/${microUsdToDollars(eu.monthly_limit_micro_usd)}`, note: 'overage', badge: 'real' }
        : { amount: null, note: 'API spend — unavailable', badge: 'na' }} />
  {/if}
  {#if codex}
    <ProviderCard name="OpenAI" plan="API + Codex"
      windows={[{ label: `Codex · ${codex.plan_type}`, used: codex.primary.used_percent,
        elapsed: codexElapsedFraction(codex.primary) * 100, tone: quotaTone(codex.primary.used_percent), resetsAt: codex.primary.resets_at }]}
      billed={openai
        ? { amount: microUsdToDollars(Math.round(openai.total_usd * 1_000_000)), note: 'this cycle', badge: 'real' }
        : { amount: null, note: snapshot.openai_error ? '✗ fetch failed' : 'spend — unavailable', badge: 'na' }} />
  {/if}
</div>

<style>
  .cards { padding: 2px 16px 0; display: flex; flex-direction: column; gap: 10px; }
</style>
