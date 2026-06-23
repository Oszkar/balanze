<script module lang="ts">
  import type { Tone } from '$lib/presentation/pace';
  export interface CardWindow { label: string; used: number; elapsed: number | null; tone: Tone; resetsAt: string; stale?: boolean; title?: string; }

  // Non-data states for the quota area, rendered in place of the window bars
  // (the billed row still renders underneath). Strings are resolved by the
  // caller so this stays a dumb presentational component; `data` shows windows.
  export type CardQuotaState =
    | { kind: 'data' }
    | { kind: 'error'; note: string; title: string }
    | { kind: 'notConfigured'; heading: string; hint: string; title: string }
    | { kind: 'loading'; heading: string; sub: string; title: string };
</script>

<script lang="ts">
  import UsageBar from './UsageBar.svelte';
  import Badge from './Badge.svelte';
  import { relativeReset } from '$lib/presentation/format';
  let { name, plan, windows, billed, quotaState }:
    { name: string; plan: string; windows: CardWindow[];
      billed: { amount: string | null; note: string; badge?: 'real' | 'na'; placeholder?: string; title?: string };
      quotaState?: CardQuotaState } = $props();
</script>

<div class="pcard">
  <div class="hd"><span class="name">{name}</span><span class="plan">{plan}</span></div>
  {#if quotaState && quotaState.kind === 'error'}
    <div class="qstate hatch" title={quotaState.title}>
      <span class="qs-na">unavailable</span>
      <span class="qs-sub">{quotaState.note}</span>
    </div>
  {:else if quotaState && quotaState.kind === 'notConfigured'}
    <div class="qstate notconf" title={quotaState.title}>
      <span class="qs-head">{quotaState.heading}</span>
      <span class="qs-sub">{quotaState.hint}</span>
    </div>
  {:else if quotaState && quotaState.kind === 'loading'}
    <div class="qstate skel" title={quotaState.title}>
      <div class="skelbar"></div>
      <div class="skeltext"><span class="qs-cap">{quotaState.heading}</span><span class="qs-sub">{quotaState.sub}</span></div>
    </div>
  {:else}
    {#each windows as w (w.label)}
      <div class="brow" title={w.title}>
        <div class="blabel"><span class="bl">{w.label}</span><span class="br">{w.used.toFixed(0)}% · {#if w.stale}<span class="sfb"><span aria-hidden="true">⚠</span> stale</span>{:else}{relativeReset(w.resetsAt)} left{/if}</span></div>
        <UsageBar used={w.used} elapsed={w.elapsed} tone={w.tone} />
      </div>
    {/each}
  {/if}
  <div class="billed" title={billed.title}>
    {#if billed.amount}<span class="amt">{billed.amount} <span class="cy">{billed.note}</span></span>
    {:else}<span class="amt na">{billed.placeholder ?? 'none'} <span class="cy">{billed.note}</span></span>{/if}
    {#if billed.badge}<Badge kind={billed.badge} />{/if}
  </div>
</div>

<style>
  .pcard {
    border-radius: 14px; background: var(--tile-face); box-shadow: var(--tile-elev);
    padding: 12px 14px; display: flex; flex-direction: column; gap: 10px;
    animation: rise .42s var(--ease-out) both; transition: box-shadow .18s var(--ease-out);
  }
  .pcard:hover { box-shadow: var(--tile-elev-hover); }
  .hd { display: flex; justify-content: space-between; align-items: baseline; }
  .name { font-size: 14px; font-weight: 600; } .plan { font-size: 10.5px; color: var(--faint); }
  /* Non-data quota states (cold-start / error / not-configured), mirroring
     GridView's anthState cells but inline within the card chrome. */
  .qstate { display: flex; flex-direction: column; gap: 4px; justify-content: center; min-height: 50px; cursor: help; }
  .qstate.hatch { border-radius: 10px; padding: 8px 10px;
    background-image: repeating-linear-gradient(45deg, transparent, transparent 5px, var(--hatch) 5px, var(--hatch) 6px); }
  .qs-na { font-size: var(--text-base); color: var(--faint); }
  .qs-head { font-size: var(--text-sm); font-weight: 600; color: var(--ink); }
  .qs-cap { font-size: var(--text-sm); color: var(--faint); }
  .qs-sub { font-size: var(--text-2xs); color: var(--faint); }
  .skelbar { height: 14px; border-radius: 7px; background: var(--track); box-shadow: var(--channel); animation: pulse 1.4s ease-in-out infinite; }
  .skeltext { display: flex; flex-direction: column; gap: 2px; }
  @keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: .4; } }
  .brow { display: flex; flex-direction: column; gap: 4px; cursor: help; }
  .blabel { display: flex; justify-content: space-between; font-size: 11px; gap: 8px; }
  .bl { color: var(--ink2); } .br { color: var(--faint); white-space: nowrap; font-variant-numeric: tabular-nums; }
  .br .sfb { color: var(--warn); }
  .billed { display: flex; justify-content: space-between; align-items: center; padding-top: 2px; cursor: help; }
  .amt { font-family: 'JetBrains Mono', ui-monospace, 'SF Mono', monospace; font-size: 14px; font-weight: 560; font-variant-numeric: tabular-nums; }
  /* Empty-state placeholder ("none"/"unavailable") sits in the amount slot, muted. */
  .amt.na { color: var(--faint); font-weight: 460; }
  .cy { font-family: 'Space Grotesk', system-ui, -apple-system, sans-serif; font-size: 10px; color: var(--faint); font-weight: 400; }
  @keyframes rise { from { opacity: 0; transform: translateY(8px); } to { opacity: 1; transform: translateY(0); } }
  @media (prefers-reduced-motion: reduce) { .pcard, .skelbar { animation: none; } }
</style>
