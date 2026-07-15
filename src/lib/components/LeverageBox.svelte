<script lang="ts">
  import { microUsdToDollars } from '$lib/presentation/format';
  import { PROV } from '$lib/presentation/provenance';
  let { totalMicroUsd, eventCount, error = null }:
    { totalMicroUsd: number; eventCount: number; error?: string | null } = $props();
</script>
{#if eventCount > 0}
  <div class="lev" title={PROV.leverageEstimate.title}>
    <div class="row"><span class="cap">Subscription leverage</span><span class="val">~{microUsdToDollars(totalMicroUsd)}</span></div>
    <div class="note">Claude usage at API list prices · not billed</div>
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
</style>
