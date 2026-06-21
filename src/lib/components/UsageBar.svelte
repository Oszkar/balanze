<script lang="ts">
  import { onMount } from 'svelte';
  import { paceVerdict, type Tone } from '$lib/presentation/pace';
  let { used, elapsed = null, tone = 'ok', height = 11 }:
    { used: number; elapsed?: number | null; tone?: Tone; height?: number } = $props();
  const clamp = (n: number) => Math.min(100, Math.max(0, n));
  // Pace verdict (quota used vs window elapsed) shown as a hover tooltip
  // whenever the elapsed tick is present. Raw (unclamped) fractions so an
  // over-cap window still reads honestly. Mirrors the CLI pace line.
  const verdict = $derived(elapsed == null ? null : paceVerdict(used / 100, elapsed / 100));

  // The fill grows from 0 to its target on first paint, so the gauge reads as
  // "settling" when the popover opens rather than snapping. Reduced-motion ->
  // start at the target. Width-only transition stays on the compositor.
  let settled = $state(false);
  onMount(() => {
    if (window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) { settled = true; return; }
    requestAnimationFrame(() => (settled = true));
  });
  const fillW = $derived(settled ? clamp(used) : 0);
</script>

<div
  class="track"
  style="height:{height}px"
  role={verdict ? 'img' : undefined}
  aria-label={verdict ? `Pace: ${verdict.text}` : undefined}
  title={verdict ? `Pace: ${verdict.text}` : undefined}
>
  <div class="fill" style="width:{fillW}%; background:var(--{tone})"></div>
  {#if elapsed != null}
    <div class="tick" style="left:{clamp(elapsed)}%"></div>
  {/if}
</div>

<style>
  .track { position: relative; border-radius: 6px; background: var(--track); box-shadow: var(--channel); }
  .fill { position: absolute; inset: 0 auto 0 0; border-radius: 6px; transition: width .55s var(--ease-out); }
  /* Elapsed marker: one centred, rounded needle overhanging the track
     symmetrically - uniform at every position, nothing to misalign. */
  .tick {
    position: absolute; top: -4px; bottom: -4px; width: 2px;
    transform: translateX(-50%); background: var(--ink); border-radius: 2px;
  }
  @media (prefers-reduced-motion: reduce) { .fill { transition: none; } }
</style>
