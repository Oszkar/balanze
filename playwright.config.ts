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
  timeout: 180_000,
  expect: { timeout: 30_000 },
  reporter: 'list',
  use: {
    baseURL: 'http://localhost:1430',
    navigationTimeout: 120_000,
    actionTimeout: 30_000,
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'bun run gallery:serve',
    url: 'http://localhost:1430/gallery.html',
    reuseExistingServer: !process.env.CI,
    timeout: 180_000,
  },
});
