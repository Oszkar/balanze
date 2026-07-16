<script lang="ts">
  import DensityToggle from './DensityToggle.svelte';
  let { view = $bindable('cards'), refreshing = false, onRefresh, onSettings }:
    { view?: 'grid' | 'cards'; refreshing?: boolean; onRefresh: () => void; onSettings: () => void } = $props();
</script>
<div class="hd">
  <div class="name">balanze</div>
  <div class="right">
    <DensityToggle bind:view />
    <button class="icon" class:refreshing type="button" aria-label={refreshing ? 'Refreshing usage' : 'Refresh now'} title={refreshing ? 'Refreshing usage' : 'Refresh now'} disabled={refreshing} onclick={onRefresh}><span aria-hidden="true">↻</span></button>
    <button class="icon" type="button" aria-label="Settings" title="Settings" onclick={onSettings}><span aria-hidden="true">⚙</span></button>
  </div>
</div>
{#if refreshing}<span class="sr-only" role="status">Refreshing usage…</span>{/if}
<style>
  .hd { display: flex; justify-content: space-between; align-items: flex-end; padding: 15px 16px 11px; }
  .name { font-size: 18px; font-weight: 600; letter-spacing: -.02em; }
  .right { display: flex; align-items: center; gap: 8px; }
  .icon { display: inline-grid; place-items: center; min-width: var(--control-target-min); min-height: var(--control-target-min); background: none; border: none; color: var(--faint); cursor: pointer; font-size: 14px; padding: 2px; border-radius: 4px; transition: color .15s var(--ease-out); }
  .icon:hover { color: var(--ink); }
  .icon:focus-visible { outline: 2px solid var(--ink2); outline-offset: 2px; }
  .icon:disabled { opacity: .5; cursor: default; }
  .icon.refreshing span { animation: spin .8s linear infinite; }
  @keyframes spin { to { transform: rotate(1turn); } }
  @media (prefers-reduced-motion: reduce) { .icon.refreshing span { animation: none; } }
  .sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden;
    clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
</style>
