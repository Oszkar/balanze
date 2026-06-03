<script lang="ts">
  import { formatBurn } from '$lib/presentation/format';
  let { tokensPerMin }: { tokensPerMin: number | null } = $props();
</script>
<div class="legend">
  <span class="l">▸ fill = used · ▏tick = elapsed</span>
  <span class="b">burn 30m {formatBurn(tokensPerMin)}</span>
</div>
<style>
  .legend { display: flex; justify-content: space-between; align-items: center; padding: 11px 16px 0; }
  .l { font-size: 9.5px; color: var(--faint); }
  .b { font-size: 9.5px; color: var(--faint); }
</style>
