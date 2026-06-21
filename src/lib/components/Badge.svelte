<script lang="ts">
  import type { BadgeKind } from '$lib/presentation/provenance';
  let { kind }: { kind: BadgeKind } = $props();
  const TEXT: Record<BadgeKind, string> = { real: 'REAL $', quota: 'QUOTA %', est: 'EST', na: 'N/A' };
</script>
<span class="badge {kind}">{TEXT[kind]}</span>
<style>
  .badge { font-size: var(--text-2xs); font-weight: 700; letter-spacing: .05em; padding: 1.5px 6px; border-radius: 10px; white-space: nowrap; }
  /* Trim the leading so the uppercase label sits optically centred in the pill -
     without it the glyphs ride high, leaving a gap below. @supports-guarded so
     older WebKit (< Safari 18.2) keeps the current look rather than regressing. */
  @supports (text-box-trim: trim-both) {
    .badge { display: inline-block; line-height: 1; padding: 4px 7px; text-box-trim: trim-both; text-box-edge: cap alphabetic; }
  }
  .real { background: var(--real); color: #fff; }
  .quota { border: 1.3px solid var(--ink2); color: var(--ink2); }
  .est { border: 1.3px dashed var(--faint); color: var(--faint); }
  .na { border: 1.3px dashed var(--faint); color: var(--faint); }
</style>
