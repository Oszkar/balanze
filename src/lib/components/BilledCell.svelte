<script lang="ts">
  let { amount = null, note, title, hatch = false, placeholder = 'unavailable' }:
    { amount?: string | null; note: string; title: string; hatch?: boolean; placeholder?: string } = $props();
</script>

<div class="cell bcell" class:hatch {title}>
  {#if amount}<span class="amt">{amount}</span>{:else}<span class="na">{placeholder}</span>{/if}
  <span class="note">{note}</span>
</div>

<style>
  .cell { border: 1.4px solid var(--tile-border); border-radius: 11px; background: var(--tile-bg); cursor: help;
          padding: 9px 11px; display: flex; flex-direction: column; gap: 3px; justify-content: center; min-height: 54px; }
  .hatch { background-image: repeating-linear-gradient(45deg, transparent, transparent 5px, var(--hatch) 5px, var(--hatch) 6px); }
  .amt { font-size: 20px; font-weight: 700; line-height: 1; }
  .na { font-size: var(--text-base); color: var(--faint); }
  .note { font-size: var(--text-2xs); color: var(--faint); }
</style>
