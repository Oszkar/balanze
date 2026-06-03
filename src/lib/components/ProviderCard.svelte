<script module lang="ts">
  import type { Tone } from '$lib/presentation/pace';
  export interface CardWindow { label: string; used: number; elapsed: number | null; tone: Tone; resetsAt: string; }
</script>

<script lang="ts">
  import UsageBar from './UsageBar.svelte';
  import Badge from './Badge.svelte';
  import { relativeReset } from '$lib/presentation/format';
  let { name, plan, windows, billed }:
    { name: string; plan: string; windows: CardWindow[];
      billed: { amount: string | null; note: string; badge: 'real' | 'na' } } = $props();
</script>

<div class="pcard">
  <div class="hd"><span class="name">{name}</span><span class="plan">{plan}</span></div>
  {#each windows as w (w.label)}
    <div class="brow">
      <div class="blabel"><span class="bl">{w.label}</span><span class="br">{w.used.toFixed(0)}% · ↻ {relativeReset(w.resetsAt)}</span></div>
      <UsageBar used={w.used} elapsed={w.elapsed} tone={w.tone} />
    </div>
  {/each}
  <div class="billed">
    {#if billed.amount}<span class="amt">{billed.amount} <span class="cy">{billed.note}</span></span>
    {:else}<span class="note">{billed.note}</span>{/if}
    <Badge kind={billed.badge} />
  </div>
</div>

<style>
  .pcard { border: 1.4px solid var(--tile-border); border-radius: 12px; background: var(--tile-bg); padding: 11px 13px; display: flex; flex-direction: column; gap: 9px; }
  .hd { display: flex; justify-content: space-between; align-items: baseline; }
  .name { font-size: 14px; font-weight: 600; } .plan { font-size: 10.5px; color: var(--faint); }
  .brow { display: flex; flex-direction: column; gap: 3px; }
  .blabel { display: flex; justify-content: space-between; font-size: 11px; gap: 8px; }
  .bl { color: var(--ink2); } .br { color: var(--faint); white-space: nowrap; }
  .billed { display: flex; justify-content: space-between; align-items: center; padding-top: 2px; }
  .amt { font-size: 14px; font-weight: 600; } .cy { font-size: 10px; color: var(--faint); font-weight: 400; }
  .note { font-size: 12px; color: var(--faint); }
</style>
