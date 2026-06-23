<script lang="ts">
  import { onMount } from 'svelte';
  import type { Snapshot } from '$lib/types/snapshot';
  import { getSettings, setSettings, resizePopover } from '$lib/ipc';
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

  // `openaiEnabled` mirrors the OpenAI *billing* opt-in (the `openai_enabled`
  // setting). It gates the "paste admin key" connect CTA, NOT column visibility:
  // the column shows whenever the snapshot carries data (Codex quota or OpenAI
  // spend), so Codex scanning being on by default never forces a key CTA for an
  // Anthropic-only user. Default false (the settings default).
  let openaiEnabled = $state(false);

  // Re-read the billing opt-in from settings. Called on mount and on the
  // Settings -> usage transition so toggling `openai_enabled` takes effect
  // without reopening the popover. Fail-open: a read failure leaves the flag
  // as-is; snapshot data still surfaces the column downstream.
  async function refreshOpenaiEnabled() {
    try {
      const s = await getSettings();
      openaiEnabled = s.providers.openai_enabled;
    } catch {
      // Leave the billing opt-in as-is on a settings read failure.
    }
  }

  onMount(refreshOpenaiEnabled);

  // Size the popover window to its content. The window opens at a fixed initial
  // height (tauri.conf.json); this corrects it to hug the rendered content on
  // first paint and on every reflow (e.g. collapsing the OpenAI column), so a
  // one-provider popover is shorter than a two-provider one. The host clamps
  // and re-anchors; errors are swallowed (a resize failure must not crash the
  // popover, and the window keeps its last good size).
  let popEl: HTMLDivElement;
  let lastSentH = 0;
  onMount(() => {
    const ro = new ResizeObserver(() => {
      // Only call the host when the measured height actually changes, so a
      // reflow burst (e.g. the usage <-> settings transition) does not spray
      // redundant resize IPC calls at the backend.
      const h = Math.ceil(popEl.getBoundingClientRect().height);
      if (h === lastSentH) return;
      lastSentH = h;
      resizePopover(h).catch(() => {});
    });
    ro.observe(popEl);
    return () => ro.disconnect();
  });

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

<div class="pop" bind:this={popEl}>
  <div class="caret"></div>
  {#if mode === 'settings'}
    <SettingsView onBack={() => { mode = 'usage'; refreshOpenaiEnabled(); }} />
  {:else}
    <Header bind:view fetchedAt={snapshot.fetched_at} {onRefresh} onSettings={() => (mode = 'settings')} />
    <DegradedBanner {degraded} />
    {#if view === 'grid'}
      <GridView {snapshot} {degraded} {openaiEnabled} {onDismissOpenai} onSettings={() => (mode = 'settings')} />
    {:else}
      <CardsView {snapshot} {openaiEnabled} {degraded} />
    {/if}
    <BurnIndicator tokensPerMin={snapshot.claude_jsonl?.recent_burn_tokens_per_min ?? null} />
    <LeverageBox totalMicroUsd={cost?.total_micro_usd ?? 0} eventCount={cost?.total_event_count ?? 0}
      error={snapshot.anthropic_api_cost_error ?? snapshot.claude_jsonl_error} />
  {/if}
</div>

<style>
  /* 1px adaptive border so the window edges are visible even when the popover
     opens over a same-colored background (white-on-white / dark-on-dark). */
  .pop { width: 100%; background: var(--paper); border-radius: var(--radius);
         border: 1px solid var(--seg-border); position: relative; overflow: hidden; }
  .caret { position: absolute; top: -7px; left: 30px; width: 14px; height: 14px; background: var(--paper);
           border-left: 1px solid var(--seg-border); border-top: 1px solid var(--seg-border); transform: rotate(45deg); }
</style>
