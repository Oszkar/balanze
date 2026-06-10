<script lang="ts">
  import type { Snapshot } from '$lib/types/snapshot';
  import Header from './Header.svelte';
  import GridView from './GridView.svelte';
  import CardsView from './CardsView.svelte';
  import BurnIndicator from './BurnIndicator.svelte';
  import LeverageBox from './LeverageBox.svelte';

  let { snapshot, degraded, onRefresh }:
    { snapshot: Snapshot; degraded: Record<string, string>; onRefresh: () => void } = $props();
  let view = $state<'grid' | 'cards'>('grid');
  const cost = $derived(snapshot.anthropic_api_cost);
</script>

<div class="pop">
  <div class="caret"></div>
  <Header bind:view fetchedAt={snapshot.fetched_at} {onRefresh} />
  {#if view === 'grid'}
    <GridView {snapshot} {degraded} />
    <BurnIndicator tokensPerMin={snapshot.claude_jsonl?.recent_burn_tokens_per_min ?? null} />
  {:else}
    <CardsView {snapshot} />
  {/if}
  <LeverageBox totalMicroUsd={cost?.total_micro_usd ?? 0} eventCount={cost?.total_event_count ?? 0}
    error={snapshot.anthropic_api_cost_error ?? snapshot.claude_jsonl_error} />
</div>

<style>
  .pop { width: 100%; min-height: 100vh; background: var(--paper); border-radius: var(--radius); position: relative; overflow: hidden; }
  .caret { position: absolute; top: -7px; left: 30px; width: 14px; height: 14px; background: var(--paper);
           border-left: 1px solid var(--hair); border-top: 1px solid var(--hair); transform: rotate(45deg); }
</style>
