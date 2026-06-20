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
    <span class="pct" style="color:var(--{tone})">{pct.toFixed(0)}%</span>
    {#if badge}<span class="badge-slot"><Badge kind={badge} /></span>{/if}
  </div>
  <UsageBar {used} {elapsed} {tone} />
  <div class="meta">
    <span class:warn={stale}>{stale ? `⚠ ${staleLabel}` : `${relativeReset(resetsAt)} left`}</span>
    <span>{secondary}</span>
  </div>
</div>

<style>
  .cell { border: 1.4px solid var(--tile-border); border-radius: 11px; background: var(--tile-bg); cursor: help; }
  .qcell { padding: 9px 11px; display: flex; flex-direction: column; gap: 7px; justify-content: center; min-height: 84px; }
  .top { display: flex; align-items: center; gap: 6px; }
  .badge-slot { margin-left: auto; }
  .pct { font-size: var(--text-2xl); font-weight: 700; line-height: 1; }
  .meta { display: flex; justify-content: space-between; font-size: var(--text-2xs); color: var(--faint); }
  .meta .warn { color: var(--warn); }
  .stale { border-color: var(--warn); }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--warn); box-shadow: 0 0 0 3px color-mix(in srgb, var(--warn) 18%, transparent); }
</style>
