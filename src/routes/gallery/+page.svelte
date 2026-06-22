<script lang="ts">
  import GalleryCanvas from '$lib/gallery/GalleryCanvas.svelte';
  const dev = import.meta.env.DEV;
</script>

{#if dev}
  <GalleryCanvas />
{:else}
  <p class="prod">The states gallery is a development-only surface.</p>
{/if}

<style>
  .prod { font-family: system-ui, sans-serif; padding: 40px; color: #888; }
</style>
