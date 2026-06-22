<script lang="ts">
  import type { Tone } from '$lib/presentation/pace';
  import type { BadgeKind } from '$lib/presentation/provenance';
  import Badge from './Badge.svelte';
  import UsageBar from './UsageBar.svelte';
  import { relativeReset } from '$lib/presentation/format';
  let { pct, used, elapsed, tone, resetsAt, secondary, title, stale = false, staleLabel = 'fallback', badge = null }:
    { pct: number; used: number; elapsed: number | null; tone: Tone;
      resetsAt: string; secondary: string; title: string; stale?: boolean; staleLabel?: string; badge?: BadgeKind | null } = $props();
</script>

<div class="cell qcell" class:stale {title}>
  <div class="top">
    {#if stale}<span class="dot"></span>{/if}
    <span class="pct" style="color:var(--{tone})">{pct.toFixed(0)}<span class="unit">%</span></span>
    {#if badge}<span class="badge-slot"><Badge kind={badge} /></span>{/if}
  </div>
  <UsageBar {used} {elapsed} {tone} />
  <div class="meta">
    <span class:warn={stale}>{#if stale}<span aria-hidden="true">⚠</span> {staleLabel}{:else}{relativeReset(resetsAt)} left{/if}</span>
    <span>{secondary}</span>
  </div>
</div>

<style>
  .cell { border-radius: 12px; background: var(--tile-face); box-shadow: var(--tile-elev); cursor: help; }
  .qcell {
    padding: 11px 12px; display: flex; flex-direction: column; gap: 8px; justify-content: center; min-height: 84px;
    animation: rise .42s var(--ease-out) both; transition: box-shadow .18s var(--ease-out);
  }
  .qcell:hover { box-shadow: var(--tile-elev-hover); }
  /* Warn ring stacks on the machined elevation (replaces the old border-color cue).
     The :hover variant keeps the ring above the hover lift - .qcell:hover on its own
     would override box-shadow and drop the stale cue while hovered. */
  .stale { box-shadow: var(--tile-elev), 0 0 0 1.5px color-mix(in srgb, var(--warn) 72%, transparent); }
  .stale:hover { box-shadow: var(--tile-elev-hover), 0 0 0 1.5px color-mix(in srgb, var(--warn) 72%, transparent); }
  .top { display: flex; align-items: center; gap: 6px; }
  .pct {
    font-family: 'JetBrains Mono', ui-monospace, 'SF Mono', monospace;
    font-size: 29px; font-weight: 560; line-height: 1; letter-spacing: -.01em;
    font-variant-numeric: tabular-nums;
    /* Trim leading so the box hugs cap-to-baseline - keeps the badge optically centred. */
    text-box-trim: trim-both; text-box-edge: cap alphabetic;
  }
  .unit { font-size: 15px; font-weight: 460; opacity: .5; margin-left: 1px; }
  .badge-slot { margin-left: auto; }
  /* Fallback for engines without text-box-trim: nudge the badge to where the trim would centre it. */
  @supports not (text-box-trim: trim-both) { .badge-slot { transform: translateY(-2px); } }
  .meta { display: flex; justify-content: space-between; font-size: var(--text-2xs); color: var(--faint); font-variant-numeric: tabular-nums; }
  .meta .warn { color: var(--warn); }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--warn); box-shadow: 0 0 0 3px color-mix(in srgb, var(--warn) 18%, transparent); }
  @keyframes rise { from { opacity: 0; transform: translateY(8px); } to { opacity: 1; transform: translateY(0); } }
  @media (prefers-reduced-motion: reduce) { .qcell { animation: none; } }
</style>
