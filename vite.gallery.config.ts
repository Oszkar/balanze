import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { fileURLToPath } from 'node:url';

// Standalone, SvelteKit-free dev/build config for the states gallery
// (`bun run gallery`). Uses the plain svelte() plugin - no SvelteKit, no SSR -
// so the gallery is a fast pure-CSR page, sidestepping the SvelteKit dev SSR
// module-runner entirely. The $lib alias mirrors svelte.config.js's default so
// the gallery's shared components (GalleryFrame and the real popover
// components it pulls in) resolve identically to the app.
export default defineConfig({
  plugins: [svelte()],
  resolve: {
    alias: {
      $lib: fileURLToPath(new URL('./src/lib', import.meta.url)),
    },
  },
  // gallery.html is not the conventional index.html, so tell Vite to crawl it
  // at startup to pre-bundle deps. Without this, deps are discovered lazily on
  // the first request and Vite force-reloads mid-navigation, which aborts
  // Playwright's page.goto with net::ERR_ABORTED.
  optimizeDeps: { entries: ['gallery.html'] },
  server: { port: 1430, strictPort: true },
  build: {
    outDir: 'build-gallery',
    rollupOptions: {
      input: fileURLToPath(new URL('./gallery.html', import.meta.url)),
    },
  },
});
