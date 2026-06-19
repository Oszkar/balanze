<script lang="ts">
  import { paceVerdict, type Tone } from '$lib/presentation/pace';
  let { used, elapsed = null, tone = 'ok', height = 11 }:
    { used: number; elapsed?: number | null; tone?: Tone; height?: number } = $props();
  const clamp = (n: number) => Math.min(100, Math.max(0, n));
  // Pace verdict (quota used vs window elapsed) shown as a hover tooltip
  // whenever the elapsed tick is present. Raw (unclamped) fractions so an
  // over-cap window still reads honestly. Mirrors the CLI pace line.
  const verdict = $derived(elapsed == null ? null : paceVerdict(used / 100, elapsed / 100));
</script>

<div class="track" style="height:{height}px" title={verdict ? `Pace: ${verdict.text}` : undefined}>
  <div class="fill" style="width:{clamp(used)}%; background:var(--{tone})"></div>
  {#if elapsed != null}
    <div class="tick" style="left:{clamp(elapsed)}%"></div>
  {/if}
</div>

<style>
  .track { position: relative; border-radius: 6px; background: var(--track); }
  .fill { position: absolute; inset: 0 auto 0 0; border-radius: 6px; }
  .tick { position: absolute; top: -3px; bottom: -3px; width: 2px; background: var(--ink); }
  .tick::before {
    content: ''; position: absolute; top: -4px; left: -2px;
    border-left: 3px solid transparent; border-right: 3px solid transparent;
    border-top: 4px solid var(--ink);
  }
</style>
