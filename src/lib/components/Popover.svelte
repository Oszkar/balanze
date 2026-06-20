<script lang="ts">
  import { onMount } from 'svelte';
  import type { Snapshot } from '$lib/types/snapshot';
  import { getSettings, setSettings } from '$lib/ipc';
  import Header from './Header.svelte';
  import GridView from './GridView.svelte';
  import CardsView from './CardsView.svelte';
  import BurnIndicator from './BurnIndicator.svelte';
  import LeverageBox from './LeverageBox.svelte';
  import DegradedBanner from './DegradedBanner.svelte';
  import SettingsView from './SettingsView.svelte';

  let { snapshot, degraded, onRefresh }:
    { snapshot: Snapshot; degraded: Record<string, string>; onRefresh: () => void } = $props();
  let view = $state<'grid' | 'cards'>('grid');
  let mode = $state<'usage' | 'settings'>('usage');
  const cost = $derived(snapshot.anthropic_api_cost);

  // The OpenAI column is shown when either OpenAI billing or Codex quota is
  // enabled. Both flags reuse the existing settings schema - no new field.
  // Default to visible (true) so a settings read failure never hides a column
  // that the snapshot may carry data for.
  let openaiEnabled = $state(true);

  // Re-read the OpenAI-side provider toggles from settings. Called on mount and
  // again on the Settings -> usage transition so re-enabling either toggle
  // un-hides the column without closing and reopening the popover. Fail-open:
  // a read failure leaves the column visible (the snapshot is the source of
  // truth for what data exists; this flag only gates the CTA vs the collapse).
  async function refreshOpenaiEnabled() {
    try {
      const s = await getSettings();
      openaiEnabled = s.providers.openai_enabled || s.providers.codex_enabled;
    } catch {
      // Leave the column visible on a settings read failure.
    }
  }

  onMount(refreshOpenaiEnabled);

  // Dismiss-to-hide: disable both OpenAI-side providers via the existing
  // settings IPC, then collapse to the single-provider view. Re-enabling either
  // toggle in Settings brings the column back. No schema change.
  async function onDismissOpenai() {
    try {
      const s = await getSettings();
      await setSettings({
        ...s,
        providers: { ...s.providers, openai_enabled: false, codex_enabled: false },
      });
      openaiEnabled = false;
    } catch {
      // No-op on failure: don't crash the popover. The column simply stays.
    }
  }
</script>

<div class="pop">
  <div class="caret"></div>
  {#if mode === 'settings'}
    <SettingsView onBack={() => { mode = 'usage'; refreshOpenaiEnabled(); }} />
  {:else}
    <Header bind:view fetchedAt={snapshot.fetched_at} {onRefresh} onSettings={() => (mode = 'settings')} />
    <DegradedBanner {degraded} />
    {#if view === 'grid'}
      <GridView {snapshot} {degraded} {openaiEnabled} {onDismissOpenai} onSettings={() => (mode = 'settings')} />
      <BurnIndicator tokensPerMin={snapshot.claude_jsonl?.recent_burn_tokens_per_min ?? null} />
    {:else}
      <CardsView {snapshot} {openaiEnabled} />
    {/if}
    <LeverageBox totalMicroUsd={cost?.total_micro_usd ?? 0} eventCount={cost?.total_event_count ?? 0}
      error={snapshot.anthropic_api_cost_error ?? snapshot.claude_jsonl_error} />
  {/if}
</div>

<style>
  /* 1px adaptive border so the window edges are visible even when the popover
     opens over a same-colored background (white-on-white / dark-on-dark). */
  .pop { width: 100%; min-height: 100vh; background: var(--paper); border-radius: var(--radius);
         border: 1px solid var(--seg-border); position: relative; overflow: hidden; }
  .caret { position: absolute; top: -7px; left: 30px; width: 14px; height: 14px; background: var(--paper);
           border-left: 1px solid var(--seg-border); border-top: 1px solid var(--seg-border); transform: rotate(45deg); }
</style>
