import { test, expect } from '@playwright/test';

// Each gallery frame is one popover state. We freeze the clock BEFORE the page
// loads so the fixtures' module-load `Date.now()` and the components' relative
// "Xs ago" text are deterministic, then snapshot every frame in both themes.
// `toHaveScreenshot` disables CSS animations by default, so the cold-start
// skeleton's pulse does not flap the baseline.
const FIXED = new Date('2026-01-01T12:00:00Z');

for (const theme of ['light', 'dark'] as const) {
  test(`gallery frames - ${theme}`, async ({ page }) => {
    // Pin Date (for the fixtures' module-load `now` and the "Xs ago" text)
    // with setFixedTime only - install() fakes all timers, which interferes
    // with font loading and Vite's client. Set before navigation so the
    // page's module-load Date reads are already frozen.
    await page.clock.setFixedTime(FIXED);
    await page.goto(`/gallery.html?theme=${theme}`, { waitUntil: 'domcontentloaded' });
    await page.locator('figure.frame').first().waitFor();
    await page.evaluate(() => document.fonts.ready);
    // Images too (the header logo): `fonts.ready` does not cover them, and on a
    // cold first navigation a frame can otherwise be captured with the logo
    // still decoding, leaving a blank gap beside the wordmark.
    await page.evaluate(() =>
      Promise.all(
        Array.from(document.images).map((img) =>
          img.complete
            ? Promise.resolve()
            : new Promise((resolve) => {
                img.addEventListener('load', resolve, { once: true });
                img.addEventListener('error', resolve, { once: true });
              }),
        ),
      ),
    );

    const frames = page.locator('figure.frame');
    const count = await frames.count();
    expect(count).toBeGreaterThan(0);

    for (let i = 0; i < count; i++) {
      const frame = frames.nth(i);
      const caption = ((await frame.locator('figcaption').textContent()) ?? `frame-${i}`).trim();
      const slug = caption
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/(^-|-$)/g, '');
      await expect(frame).toHaveScreenshot(`${slug}-${theme}.png`);
    }
  });
}
