import { defineConfig, devices } from '@playwright/test';

// Visual-regression harness over the states gallery. It boots the standalone
// (SSR-free) gallery server, and the spec freezes the clock + waits for fonts
// before screenshotting every frame, so renders are deterministic. Baselines
// are committed and Playwright suffixes them per project + platform (e.g.
// *-chromium-win32.png). Regenerate with `bun run gallery:snap --update-snapshots`.
//
// Local/manual harness (run before review or a release), mirroring the repo's
// other manually-run smokes (AGENTS.md section 6). Not wired into per-PR CI yet:
// committed baselines are platform-specific, so a Linux CI run would need its
// own baselines generated in that environment.
export default defineConfig({
  testDir: 'tests/visual',
  // Serialize: the standalone Vite server transforms the whole popover
  // component tree cold on first navigation (slow on Windows). One worker lets
  // the first test warm the server's transform cache for the second, and keeps
  // two cold navigations from contending.
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  // Must exceed `use.navigationTimeout` below, or the test dies before the cold
  // first navigation is allowed to finish and that allowance is unreachable.
  timeout: 360_000,
  expect: { timeout: 30_000 },
  reporter: 'list',
  use: {
    // 127.0.0.1, not localhost: on Windows `localhost` resolves to ::1 first and
    // the Vite gallery server binds 127.0.0.1 only (see vite.gallery.config.ts),
    // so a `localhost` navigation stalls. Keep this in lockstep with the bind.
    baseURL: 'http://127.0.0.1:1430',
    // The first navigation transforms the whole popover component tree cold,
    // which on Windows can exceed 2 minutes; 120s timed out here even with a
    // warm optimizeDeps cache. Give the cold first frame room (the second theme
    // reuses the warmed transform cache and is fast).
    navigationTimeout: 300_000,
    actionTimeout: 30_000,
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'bun run gallery:serve',
    url: 'http://127.0.0.1:1430/gallery.html',
    reuseExistingServer: !process.env.CI,
    timeout: 180_000,
  },
});
