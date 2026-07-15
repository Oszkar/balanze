<script lang="ts">
  import { onMount } from 'svelte';
  import { usage } from '$lib/stores/usage.svelte';
  import { hideWindow } from '$lib/ipc';
  import Popover from '$lib/components/Popover.svelte';
  import EmptyState from '$lib/components/EmptyState.svelte';

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

<main aria-label="Balanze usage">
  <h1 class="sr-only">Balanze usage</h1>
  {#if usage.loading}
    <div class="state">Loading...</div>
  {:else if usage.snapshot}
    <Popover snapshot={usage.snapshot} degraded={usage.degraded} onRefresh={() => usage.refresh()} />
  {:else}
    <EmptyState
      title="Balanze isn't responding yet"
      body="The background service may still be starting."
      detail={usage.lastError}
      actions={[{ label: 'Retry', kind: 'primary', onClick: () => void usage.refresh() }]}
    />
  {/if}
</main>

<style>
  .state { display: flex; align-items: center; justify-content: center; min-height: 100vh; color: var(--faint); font-size: 13px; }
  .sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden;
    clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
</style>
