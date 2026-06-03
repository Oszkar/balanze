<script lang="ts">
  let { view = $bindable('grid') }: { view?: 'grid' | 'cards' } = $props();
</script>
<div class="seg">
  <button class:on={view === 'grid'} onclick={() => (view = 'grid')}>▦ Grid</button>
  <button class:on={view === 'cards'} onclick={() => (view = 'cards')}>▤ Cards</button>
</div>
<style>
  .seg { display: flex; border: 1.5px solid var(--seg-border); border-radius: 8px; overflow: hidden; }
  button { font: inherit; font-size: 11px; font-weight: 600; padding: 4px 9px; border: none; cursor: pointer; line-height: 1.4; white-space: nowrap; background: transparent; color: var(--faint); }
  .on { background: var(--seg-on); color: var(--seg-on-text); }
</style>
