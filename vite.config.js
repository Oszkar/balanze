import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [sveltekit({
    // Tauri serves a static SPA without a Node.js server.
    adapter: adapter({ fallback: "index.html" }),
    preprocess: vitePreprocess(),
    version: { pollInterval: 0 },
  })],

  test: {
    include: ['src/**/*.{test,spec}.ts', 'scripts/**/*.{test,spec}.ts'],
    environment: 'node',
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || "127.0.0.1",
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri` and the Cargo workspace
      //    `target/` at the repo root (100k+ files; crawling it on startup
      //    pushes SvelteKit's dev SSR past Vite's 60s module-runner timeout)
      ignored: ["**/src-tauri/**", "**/target/**"],
    },
  },
}));
