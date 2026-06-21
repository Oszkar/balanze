<script lang="ts">
  import type { BadgeKind } from '$lib/presentation/provenance';
  import Badge from './Badge.svelte';
  let { amount = null, note, title, hatch = false, placeholder = 'unavailable', badge = null }:
    { amount?: string | null; note: string; title: string; hatch?: boolean; placeholder?: string; badge?: BadgeKind | null } = $props();
</script>

<div class="cell bcell" class:hatch {title}>
  {#if badge}<span class="badge-slot"><Badge kind={badge} /></span>{/if}
  {#if amount}<span class="amt">{amount}</span>{:else}<span class="na">{placeholder}</span>{/if}
  <span class="note">{note}</span>
</div>

<style>
  .cell { border-radius: 12px; background: var(--tile-face); box-shadow: var(--tile-elev); cursor: help;
          padding: 9px 11px; display: flex; flex-direction: column; gap: 3px; justify-content: center; min-height: 54px;
          transition: box-shadow .18s var(--ease-out); }
  .cell:hover { box-shadow: var(--tile-elev-hover); }
  /* Layer the unavailable-state hatch over the machined face, not instead of it. */
  .hatch { background-image: repeating-linear-gradient(45deg, transparent, transparent 5px, var(--hatch) 5px, var(--hatch) 6px), var(--tile-face); }
  .amt { font-family: 'JetBrains Mono', ui-monospace, 'SF Mono', monospace; font-size: 17px; font-weight: 560; line-height: 1; font-variant-numeric: tabular-nums; }
  .na { font-size: var(--text-base); color: var(--faint); }
  .note { font-size: var(--text-2xs); color: var(--faint); }
  .badge-slot { align-self: flex-end; margin-bottom: -2px; }
</style>
