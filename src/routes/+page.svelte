<script lang="ts">
  import { onMount } from 'svelte';
  import { usage } from '$lib/stores/usage.svelte';
  import { hideWindow } from '$lib/ipc';
  import Popover from '$lib/components/Popover.svelte';

  onMount(() => {
    usage.init();
    // ESC dismisses the popover (same as clicking away / blur-hide). Outside
    // the Tauri runtime (browser dev) hide() rejects - swallow it.
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') void hideWindow().catch(() => {});
    };
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
      usage.destroy();
    };
  });
</script>

{#if usage.loading}
  <div class="state">Loading…</div>
{:else if usage.snapshot}
  <Popover snapshot={usage.snapshot} degraded={usage.degraded} onRefresh={() => usage.refresh()} />
{:else}
  <div class="state">No data {usage.lastError ? `· ${usage.lastError}` : ''}</div>
{/if}

<style>
  .state { display: flex; align-items: center; justify-content: center; min-height: 100vh; color: var(--faint); font-size: 13px; font-family: 'Inter', system-ui, sans-serif; }
</style>
