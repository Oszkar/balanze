<script lang="ts">
  import { untrack } from 'svelte';
  import type { Snapshot } from '$lib/types/snapshot';
  import Header from '$lib/components/Header.svelte';
  import GridView from '$lib/components/GridView.svelte';
  import CardsView from '$lib/components/CardsView.svelte';
  import SettingsView from '$lib/components/SettingsView.svelte';
  import EmptyState from '$lib/components/EmptyState.svelte';
  import BurnIndicator from '$lib/components/BurnIndicator.svelte';
  import LeverageBox from '$lib/components/LeverageBox.svelte';

  // One gallery frame: a caption plus the real popover chrome (Header + view),
  // wrapped in the `.pop` shell at the fixed 360px width. Composed exactly as
  // Popover.svelte does so each frame matches the shipped layout.
  let { label, view, snapshot, degraded = {}, openaiEnabled = false, empty }: {
    label: string;
    view: 'grid' | 'cards' | 'settings' | 'empty';
    snapshot?: Snapshot;
    degraded?: Record<string, string>;
    openaiEnabled?: boolean;
    empty?: { title: string; body?: string; detail?: string | null; actions?: { label: string; kind?: 'primary' | 'secondary' }[] };
  } = $props();

  // The Header's segmented picker flips this local view, so each grid/cards frame
  // is a live mini-popover. The toggle only swaps the local render - no IPC, no
  // shared state. Seeded once from the descriptor's view (props are fixed per
  // frame, so `untrack` makes the initial-value-only read explicit).
  let activeView = $state<'grid' | 'cards'>(untrack(() => (view === 'cards' ? 'cards' : 'grid')));

  // Every interactive callback is inert in the gallery: refresh, settings, and
  // dismiss do nothing (and Settings writes are neutralized by the route's
  // mockIPC), so clicking around can never touch real data.
  const noop = () => {};
</script>

<figure class="frame">
  <figcaption>{label}</figcaption>
  <div class="pop">
    <div class="caret"></div>
    {#if view === 'settings'}
      <SettingsView onBack={noop} />
    {:else if view === 'empty' && empty}
      <EmptyState title={empty.title} body={empty.body} detail={empty.detail} actions={empty.actions ?? []} />
    {:else if snapshot}
      <Header bind:view={activeView} fetchedAt={snapshot.fetched_at} onRefresh={noop} onSettings={noop} />
      {#if activeView === 'cards'}
        <CardsView {snapshot} {openaiEnabled} {degraded} />
        <LeverageBox
          totalMicroUsd={snapshot.anthropic_api_cost?.total_micro_usd ?? 0}
          eventCount={snapshot.anthropic_api_cost?.total_event_count ?? 0}
          error={snapshot.anthropic_api_cost_error ?? snapshot.claude_jsonl_error}
        />
      {:else}
        <GridView {snapshot} {degraded} {openaiEnabled} onDismissOpenai={noop} onSettings={noop} />
        <BurnIndicator tokensPerMin={snapshot.claude_jsonl?.recent_burn_tokens_per_min ?? null} />
        <LeverageBox
          totalMicroUsd={snapshot.anthropic_api_cost?.total_micro_usd ?? 0}
          eventCount={snapshot.anthropic_api_cost?.total_event_count ?? 0}
          error={snapshot.anthropic_api_cost_error ?? snapshot.claude_jsonl_error}
        />
      {/if}
    {/if}
  </div>
</figure>

<style>
  .frame { margin: 0; display: flex; flex-direction: column; gap: 8px; width: 360px; }
  figcaption { font-size: 12px; font-weight: 600; color: var(--faint); }
  /* Mirrors Popover.svelte's .pop / .caret so each frame reads as the real window. */
  .pop { width: 360px; background: var(--paper); border-radius: var(--radius);
         border: 1px solid var(--seg-border); position: relative; }
  .caret { position: absolute; top: -7px; left: 30px; width: 14px; height: 14px; background: var(--paper);
           border-left: 1px solid var(--seg-border); border-top: 1px solid var(--seg-border); transform: rotate(45deg); }
</style>
