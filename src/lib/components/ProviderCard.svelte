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
    | { kind: 'loading'; heading: string; sub: string; title: string }
    | { kind: 'connect'; label: string; cta: string; hint: string };
</script>

<script lang="ts">
  import UsageBar from './UsageBar.svelte';
  import Badge from './Badge.svelte';
  import { relativeReset } from '$lib/presentation/format';
  import { OPENAI_COL_COPY } from '$lib/presentation/quotaCopy';
  // `billed` is optional: the connect/error states suppress the spend row (the
  // state block spans the card body, mirroring GridView's two-row span). `onDismiss`
  // renders the × that collapses the column; `onConnect` wires the connect CTA.
  let { name, plan, windows, billed, quotaState, onDismiss, onConnect }:
    { name: string; plan: string; windows: CardWindow[];
      billed?: { amount: string | null; note: string; badge?: 'real' | 'na'; placeholder?: string; title?: string };
      quotaState?: CardQuotaState; onDismiss?: () => void; onConnect?: () => void } = $props();
</script>

<div class="pcard">
  <div class="hd">
    <span class="name">{name}</span>
    <span class="hd-right">
      <span class="plan">{plan}</span>
      {#if onDismiss}<button class="dismiss" type="button" aria-label={OPENAI_COL_COPY.dismiss.aria} title={OPENAI_COL_COPY.dismiss.title} onclick={() => onDismiss?.()}>×</button>{/if}
    </span>
  </div>
  {#if quotaState && quotaState.kind === 'connect'}
    <div class="qstate connect">
      <span class="connect-label">{quotaState.label}</span>
      <button class="connect-btn" type="button" onclick={() => onConnect?.()}>{quotaState.cta}</button>
      <span class="connect-hint">{quotaState.hint}</span>
    </div>
  {:else if quotaState && quotaState.kind === 'error'}
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
  {#if billed}
    <div class="billed" title={billed.title}>
      {#if billed.amount}<span class="amt">{billed.amount} <span class="cy">{billed.note}</span></span>
      {:else}<span class="amt na">{billed.placeholder ?? 'none'} <span class="cy">{billed.note}</span></span>{/if}
      {#if billed.badge}<Badge kind={billed.badge} />{/if}
    </div>
  {/if}
</div>

<style>
  .pcard {
    border-radius: 14px; background: var(--tile-face); box-shadow: var(--tile-elev);
    padding: 12px 14px; display: flex; flex-direction: column; gap: 10px;
    animation: rise .42s var(--ease-out) both; transition: box-shadow .18s var(--ease-out);
  }
  .pcard:hover { box-shadow: var(--tile-elev-hover); }
  .hd { display: flex; justify-content: space-between; align-items: baseline; }
  .hd-right { display: flex; align-items: baseline; gap: 6px; }
  .name { font-size: 14px; font-weight: 600; } .plan { font-size: 10.5px; color: var(--faint); }
  /* Dismiss × in the card header (OpenAI column only), mirroring GridView's. */
  .dismiss { background: none; border: none; color: var(--faint); cursor: pointer; font-size: var(--text-base);
    line-height: 1; padding: 0 2px; align-self: center; }
  .dismiss:hover { color: var(--ink); }
  .dismiss:focus-visible { outline: 2px solid var(--ink2); outline-offset: 1px; border-radius: 4px; }
  /* Non-data quota states (cold-start / error / not-configured / connect),
     mirroring GridView's cells but inline within the card chrome. */
  .qstate { display: flex; flex-direction: column; gap: 4px; justify-content: center; min-height: 50px; cursor: help; }
  /* Connect CTA: billing opted in, nothing to show yet. Centered, non-help cursor
     (it is actionable, not a tooltip). */
  .qstate.connect { align-items: center; gap: 6px; cursor: default; }
  .connect-label { font-size: var(--text-sm); color: var(--faint); }
  .connect-btn { font-size: var(--text-sm); font-weight: 600; padding: 5px 12px; border-radius: 8px;
    border: 1px solid var(--seg-border); background: var(--seg-on); color: var(--seg-on-text); cursor: pointer; }
  .connect-btn:hover { opacity: .88; }
  .connect-btn:focus-visible { outline: 2px solid var(--ink2); outline-offset: 2px; }
  .connect-hint { font-size: var(--text-2xs); color: var(--faint); }
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
