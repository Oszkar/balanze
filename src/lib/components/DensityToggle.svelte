<script lang="ts">
  let { view = $bindable('grid') }: { view?: 'grid' | 'cards' } = $props();
</script>
<div class="seg">
  <button type="button" aria-pressed={view === 'grid'} class:on={view === 'grid'} onclick={() => (view = 'grid')}>Grid</button>
  <button type="button" aria-pressed={view === 'cards'} class:on={view === 'cards'} onclick={() => (view = 'cards')}>Cards</button>
</div>
<style>
  /* Machined segmented control: recessed track, raised active pill. */
  .seg { display: inline-flex; padding: 2px; border-radius: 9px; background: var(--track); box-shadow: var(--channel); }
  button { font: inherit; font-size: 11px; font-weight: 600; padding: 3px 10px; border: none; border-radius: 7px; cursor: pointer;
    line-height: 1.4; white-space: nowrap; background: transparent; color: var(--faint);
    transition: color .15s var(--ease-out), box-shadow .18s var(--ease-out); }
  button:hover:not(.on) { color: var(--ink); }
  button:focus-visible { outline: 2px solid var(--ink2); outline-offset: 1px; }
  .on { background: var(--paper); color: var(--ink); box-shadow: var(--tile-elev); }
</style>
