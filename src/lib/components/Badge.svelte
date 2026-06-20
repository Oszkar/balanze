<script lang="ts">
  import type { BadgeKind } from '$lib/presentation/provenance';
  let { kind }: { kind: BadgeKind } = $props();
  const TEXT: Record<BadgeKind, string> = { real: 'REAL $', quota: 'QUOTA %', est: 'EST', na: 'N/A' };
</script>
<span class="badge {kind}">{TEXT[kind]}</span>
<style>
  .badge { font-size: var(--text-2xs); font-weight: 700; letter-spacing: .05em; padding: 1.5px 6px; border-radius: 10px; white-space: nowrap; }
  .real { background: var(--real); color: #fff; }
  .quota { border: 1.3px solid var(--ink2); color: var(--ink2); }
  .est { border: 1.3px dashed var(--faint); color: var(--faint); }
  .na { border: 1.3px dashed var(--faint); color: var(--faint); }
</style>
