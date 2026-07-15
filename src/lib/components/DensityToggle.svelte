<script lang="ts">
  let { view = $bindable('grid') }: { view?: 'grid' | 'cards' } = $props();
</script>
<div class="seg">
  <button type="button" aria-pressed={view === 'grid'} class:on={view === 'grid'} onclick={() => (view = 'grid')}>Compact</button>
  <button type="button" aria-pressed={view === 'cards'} class:on={view === 'cards'} onclick={() => (view = 'cards')}>Details</button>
</div>
<style>
  /* Machined segmented control: recessed track, raised active pill. */
  .seg { display: inline-flex; padding: 2px; border-radius: 9px; background: var(--track); box-shadow: var(--channel); }
  button { font: inherit; font-size: var(--text-xs); font-weight: 600; min-height: var(--control-target-min); padding: 3px 10px; border: none; border-radius: 7px; cursor: pointer;
    line-height: 1.4; white-space: nowrap; background: transparent; color: var(--faint);
    transition: color .15s var(--ease-out), box-shadow .18s var(--ease-out); }
  button:hover:not(.on) { color: var(--ink); }
  button:focus-visible { outline: 2px solid var(--ink2); outline-offset: 1px; }
  .on { background: var(--paper); color: var(--ink); box-shadow: var(--tile-elev); }
</style>
