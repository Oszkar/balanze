import { defineConfig } from 'vite';
import { svelte, vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { fileURLToPath } from 'node:url';

// Standalone, SvelteKit-free dev/build config for the states gallery
// (`bun run gallery`). Uses the plain svelte() plugin - no SvelteKit, no SSR -
// so the gallery is a fast pure-CSR page, sidestepping the SvelteKit dev SSR
// module-runner entirely. Shared components resolve through package.json's
// #lib subpath imports, identically to the app.
export default defineConfig({
  plugins: [svelte({ preprocess: vitePreprocess() })],
  // gallery.html is not the conventional index.html, so tell Vite to crawl it
  // at startup to pre-bundle deps. Without this, deps are discovered lazily on
  // the first request and Vite force-reloads mid-navigation, which aborts
  // Playwright's page.goto with net::ERR_ABORTED.
  optimizeDeps: { entries: ['gallery.html'] },
  // Bind 127.0.0.1 explicitly (not the default `localhost`), mirroring
  // vite.config.js. On Windows, `localhost` resolves to ::1 first and Vite 8's
  // module runner hangs on the IPv6 socket, so the server prints "ready" but
  // never serves - which silently broke `bun run gallery:snap` (the Playwright
  // visual harness) on Windows until this pin.
  server: { host: '127.0.0.1', port: 1430, strictPort: true },
  build: {
    outDir: 'build-gallery',
    rollupOptions: {
      input: fileURLToPath(new URL('./gallery.html', import.meta.url)),
    },
  },
});
