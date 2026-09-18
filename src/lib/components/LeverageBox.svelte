<script lang="ts">
  import { microUsdToDollars } from '$lib/presentation/format';
  import { PROV } from '$lib/presentation/provenance';
  import type { Cost } from '$lib/types/snapshot';
  let { cost, error = null }: { cost: Cost | null; error?: string | null } = $props();
  const pricedCount = $derived(cost?.per_model.reduce((sum, row) => sum + row.event_count, 0) ?? 0);
  // Partial means usage went unpriced: a model missing from the price table, or an
  // event with no model name. Counting events instead would trip on Claude Code's
  // zero-token `<synthetic>` turns, which are never priced and never cost anything.
  const partial = $derived(cost !== null && (cost.skipped_models.length > 0 || cost.unparsed_event_count > 0));
</script>
{#if cost && cost.total_event_count > 0}
  <div class="lev" title={PROV.leverageEstimate.title}>
    <div class="row"><span class="cap">Subscription leverage</span><span class="val">{pricedCount > 0 || !partial ? `~${microUsdToDollars(cost.total_micro_usd)}` : 'Unavailable'}</span></div>
    <div class="note">This month at API list prices · not billed</div>
    {#if partial}
      <div class="coverage">{pricedCount > 0 ? 'Partial estimate' : 'No priced usage'} · {pricedCount} of {cost.total_event_count} events priced</div>
      {#if cost.skipped_models.length > 0}
        <div class="note">Missing prices: {cost.skipped_models.join(', ')}</div>
      {/if}
      {#if cost.unparsed_event_count > 0}
        <div class="note">Missing model name: {cost.unparsed_event_count} {cost.unparsed_event_count === 1 ? 'event' : 'events'}</div>
      {/if}
    {/if}
  </div>
{:else if error}
  <div class="lev"><div class="note">Subscription leverage: ✗ {error}</div></div>
{/if}
<style>
  .lev { margin: 11px 16px 15px; border: 1.4px dashed var(--lev-border); border-radius: 10px; padding: 9px 12px; background: var(--lev-bg); }
  .row { display: flex; justify-content: space-between; align-items: center; }
  .cap { font-size: var(--text-2xs); letter-spacing: .05em; text-transform: uppercase; color: var(--faint); }
  .val { font-family: 'JetBrains Mono', ui-monospace, 'SF Mono', monospace; font-size: 15px; font-weight: 560; font-variant-numeric: tabular-nums; }
  .note { font-size: var(--text-2xs); color: var(--faint); margin-top: 2px; }
  .note, .coverage { overflow-wrap: anywhere; }
  .coverage { font-size: var(--text-2xs); color: var(--ink); margin-top: 5px; }
</style>
