<script lang="ts">
  import type { Snapshot } from '$lib/types/snapshot';
  import GridView from '$lib/components/GridView.svelte';
  import CardsView from '$lib/components/CardsView.svelte';
  import SettingsView from '$lib/components/SettingsView.svelte';
  import BurnIndicator from '$lib/components/BurnIndicator.svelte';
  import LeverageBox from '$lib/components/LeverageBox.svelte';

  // One gallery frame: a caption plus the real sub-view, wrapped in the popover's
  // `.pop` chrome at the fixed 360px width. Grid/cards are composed exactly as
  // Popover.svelte does so each frame matches the shipped layout.
  let { label, view, snapshot, degraded = {}, openaiEnabled = false }: {
    label: string;
    view: 'grid' | 'cards' | 'settings';
    snapshot?: Snapshot;
    degraded?: Record<string, string>;
    openaiEnabled?: boolean;
  } = $props();

  const noop = () => {};
</script>

<figure class="frame">
  <figcaption>{label}</figcaption>
  <div class="pop">
    <div class="caret"></div>
    {#if view === 'settings'}
      <SettingsView onBack={noop} />
    {:else if snapshot && view === 'cards'}
      <CardsView {snapshot} {openaiEnabled} {degraded} />
      <LeverageBox
        totalMicroUsd={snapshot.anthropic_api_cost?.total_micro_usd ?? 0}
        eventCount={snapshot.anthropic_api_cost?.total_event_count ?? 0}
        error={snapshot.anthropic_api_cost_error ?? snapshot.claude_jsonl_error}
      />
    {:else if snapshot}
      <GridView {snapshot} {degraded} {openaiEnabled} onDismissOpenai={noop} onSettings={noop} />
      <BurnIndicator tokensPerMin={snapshot.claude_jsonl?.recent_burn_tokens_per_min ?? null} />
      <LeverageBox
        totalMicroUsd={snapshot.anthropic_api_cost?.total_micro_usd ?? 0}
        eventCount={snapshot.anthropic_api_cost?.total_event_count ?? 0}
        error={snapshot.anthropic_api_cost_error ?? snapshot.claude_jsonl_error}
      />
    {/if}
  </div>
</figure>

<style>
  .frame { margin: 0; display: flex; flex-direction: column; gap: 8px; width: 360px; }
  figcaption { font-size: 12px; font-weight: 600; color: var(--faint); font-family: 'Inter', system-ui, sans-serif; }
  /* Mirrors Popover.svelte's .pop / .caret so each frame reads as the real window. */
  .pop { width: 360px; background: var(--paper); border-radius: var(--radius);
         border: 1px solid var(--seg-border); position: relative; }
  .caret { position: absolute; top: -7px; left: 30px; width: 14px; height: 14px; background: var(--paper);
           border-left: 1px solid var(--seg-border); border-top: 1px solid var(--seg-border); transform: rotate(45deg); }
</style>
