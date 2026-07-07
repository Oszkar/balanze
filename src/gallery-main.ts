// Standalone, SvelteKit-free entry for the states gallery. Mounts GalleryCanvas
// through a plain Vite + svelte pipeline (vite.gallery.config.ts) with no SSR -
// so it boots fast and is immune to the SvelteKit dev-server SSR module-runner
// that can hang on Windows. Run via `bun run gallery`.
import { mount } from 'svelte';
import GalleryCanvas from './lib/gallery/GalleryCanvas.svelte';

const target = document.getElementById('gallery');
if (!target) throw new Error('gallery: #gallery mount target not found');
mount(GalleryCanvas, { target });
