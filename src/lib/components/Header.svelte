<script lang="ts">
  import DensityToggle from './DensityToggle.svelte';
  let { view = $bindable('grid'), fetchedAt, onRefresh, onSettings }:
    { view?: 'grid' | 'cards'; fetchedAt: string; onRefresh: () => void; onSettings: () => void } = $props();
  const ago = $derived.by(() => {
    const s = Math.max(0, Math.round((Date.now() - new Date(fetchedAt).getTime()) / 1000));
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s / 60)}m`;
    return `${Math.floor(s / 3600)}h`;
  });
</script>
<div class="hd">
  <div><div class="name">balanze</div><div class="sub">{view === 'grid' ? 'measured usage' : 'by provider'} · {ago} ago</div></div>
  <div class="right">
    <DensityToggle bind:view />
    <button class="icon" title="Refresh now" onclick={onRefresh}>↻</button>
    <button class="icon" title="Settings" onclick={onSettings}>⚙</button>
  </div>
</div>
<style>
  .hd { display: flex; justify-content: space-between; align-items: flex-end; padding: 15px 16px 11px; }
  .name { font-size: 18px; font-weight: 700; letter-spacing: -.01em; }
  .sub { font-size: 11px; color: var(--faint); margin-top: 1px; }
  .right { display: flex; align-items: center; gap: 8px; }
  .icon { background: none; border: none; color: var(--faint); cursor: pointer; font-size: 14px; padding: 2px; }
  .icon:disabled { opacity: .5; cursor: default; }
</style>
