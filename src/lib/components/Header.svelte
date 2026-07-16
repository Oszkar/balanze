<script lang="ts">
  import DensityToggle from './DensityToggle.svelte';
  // Imported (not /logo.svg from static/) so it resolves under both SvelteKit
  // and the standalone gallery, which runs plain Vite and does not serve static/.
  import logoUrl from '$lib/assets/logo.svg?url';
  let { view = $bindable('cards'), refreshing = false, onRefresh, onSettings }:
    { view?: 'grid' | 'cards'; refreshing?: boolean; onRefresh: () => void; onSettings: () => void } = $props();
</script>
<div class="hd">
  <div class="brand">
    <!-- Decorative: the wordmark beside it already names the app, so alt="" keeps
         a screen reader from announcing "Balanze" twice. -->
    <img class="mark" src={logoUrl} alt="" width="17" height="18" />
    <div class="name">balanze</div>
  </div>
  <div class="right">
    <DensityToggle bind:view />
    <button class="icon" class:refreshing type="button" aria-label={refreshing ? 'Refreshing usage' : 'Refresh now'} title={refreshing ? 'Refreshing usage' : 'Refresh now'} disabled={refreshing} onclick={onRefresh}><span aria-hidden="true">↻</span></button>
    <button class="icon" type="button" aria-label="Settings" title="Settings" onclick={onSettings}><span aria-hidden="true">⚙</span></button>
  </div>
</div>
{#if refreshing}<span class="sr-only" role="status">Refreshing usage...</span>{/if}
<style>
  .hd { display: flex; justify-content: space-between; align-items: flex-end; padding: 15px 16px 11px; }
  .brand { display: flex; align-items: center; gap: 7px; }
  /* Optically aligned to the wordmark's cap height rather than its box. */
  .mark { display: block; height: 18px; width: auto; flex: none; }
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
