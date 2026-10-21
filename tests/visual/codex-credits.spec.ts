import { test, expect } from '@playwright/test';

for (const theme of ['light', 'dark']) {
  test(`Codex credits render only when a balance is available - ${theme}`, async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-01-01T12:00:00Z'));
    await page.goto(`/gallery.html?credits-regression&theme=${theme}`);
    for (const view of ['grid', 'cards']) {
      for (const state of ['balance', 'zero', 'unknown', 'null', 'absent']) {
        const frame = page.locator('figure.frame').filter({
          has: page.getByText(`${view} - credits ${state}`, { exact: true }),
        });
        const popover = frame.locator('.pop');
        if (state === 'balance') {
          await expect(popover).toContainText('62,500 credits');
        } else {
          await expect(popover).not.toContainText(/\bcredits?\b/);
        }
        // Weekly-only plans must not claim a five-hour quota in the tooltip.
        await expect(popover.locator('[title="Server-reported quota · Codex CLI rate-limit · plan-dependent windows"]')).toHaveCount(1);
      }
    }
  });
}
