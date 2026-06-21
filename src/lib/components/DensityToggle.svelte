<script lang="ts">
  let { view = $bindable('grid') }: { view?: 'grid' | 'cards' } = $props();
</script>
<div class="seg">
  <button type="button" aria-pressed={view === 'grid'} class:on={view === 'grid'} onclick={() => (view = 'grid')}>▦ Grid</button>
  <button type="button" aria-pressed={view === 'cards'} class:on={view === 'cards'} onclick={() => (view = 'cards')}>▤ Cards</button>
</div>
<style>
  .seg { display: flex; border: 1.5px solid var(--seg-border); border-radius: 8px; overflow: hidden; }
  button { font: inherit; font-size: 11px; font-weight: 600; padding: 4px 9px; border: none; cursor: pointer; line-height: 1.4; white-space: nowrap; background: transparent; color: var(--faint); }
  button:hover:not(.on) { color: var(--ink); }
  button:focus-visible { outline: 2px solid var(--ink2); outline-offset: -2px; }
  .on { background: var(--seg-on); color: var(--seg-on-text); }
</style>
