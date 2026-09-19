import { test, expect } from '@playwright/test';

// Each gallery frame is one popover state. We freeze the clock BEFORE the page
// loads so the fixtures' module-load `Date.now()` and the components' relative
// "Xs ago" text are deterministic, then snapshot every frame in both themes.
// `toHaveScreenshot` disables CSS animations by default, so the cold-start
// skeleton's pulse does not flap the baseline.
const FIXED = new Date('2026-01-01T12:00:00Z');

for (const theme of ['light', 'dark'] as const) {
  test(`Anthropic quota freshness - ${theme}`, async ({ page }) => {
    await page.clock.setFixedTime(FIXED);
    await page.goto(`/gallery.html?theme=${theme}`, { waitUntil: 'domcontentloaded' });
    const frame = (label: string) => page.locator('figure.frame').filter({ has: page.getByText(label, { exact: true }) });
    for (const scenario of ['aged quota', 'passed reset']) {
      const grid = frame(`Grid - Anthropic ${scenario}`).locator('.qcell').first();
      await expect(grid).toContainText('stale');
      await expect(grid.locator('.tick')).toHaveCount(0);
      const cards = frame(`Cards - Anthropic ${scenario}`).locator('.pcard').first();
      const bars = await cards.locator('.brow').count();
      expect(bars).toBeGreaterThan(0);
      await expect(cards.locator('.sfb')).toHaveCount(bars);
      await expect(cards.locator('.tick')).toHaveCount(0);
    }
    const fallback = frame('Grid - Anthropic statusline fallback').locator('.qcell').first();
    await expect(fallback).not.toContainText('stale');
    await expect(fallback.locator('.tick')).toHaveCount(1);
    await expect(fallback).toContainText('62%');
    await page.evaluate(() => document.fonts.ready);
    // The SVG logo's intrinsic width changes the adjacent wordmark position
    // once decoded; font readiness alone does not stabilize the header.
    await page.evaluate(() => Promise.all(Array.from(document.images, (img) => img.decode())));
    for (const view of ['Grid', 'Cards']) {
      for (const scenario of ['aged quota', 'passed reset', 'statusline fallback']) {
        const label = `${view} - Anthropic ${scenario}`;
        const slug = label.toLowerCase().replace(/[^a-z0-9]+/g, '-');
        await expect(frame(label)).toHaveScreenshot(`${slug}-${theme}.png`);
      }
    }
  });
}

test('leverage distinguishes partial, unpriced, and zero-cost usage', async ({ page }) => {
  await page.clock.setFixedTime(FIXED);
  await page.goto('/gallery.html?theme=light', { waitUntil: 'domcontentloaded' });
  const leverage = (label: string) => page.locator('figure.frame').filter({
    has: page.getByText(label, { exact: true }),
  }).locator('.lev');

  const full = leverage('Cards - two providers');
  await expect(full.locator('.val')).toHaveText('~$47.30');
  await expect(full.locator('.coverage')).toHaveCount(0);
  // Zero-token placeholder turns are events nothing was left unpriced for.
  const placeholder = leverage('Cards - zero-usage placeholder turns');
  await expect(placeholder.locator('.val')).toHaveText('~$47.30');
  await expect(placeholder.locator('.coverage')).toHaveCount(0);
  const partial = leverage('Cards - partial pricing');
  await expect(partial.locator('.val')).toHaveText('~$47.30');
  await expect(partial).toContainText('Partial estimate · 340 of 400 events priced');
  await expect(partial).toContainText('Missing prices: claude-future-model');
  await expect(partial).toContainText('Missing model name: 2 events');
  const unknown = leverage('Cards - unknown model pricing');
  await expect(unknown.locator('.val')).toHaveText('Unavailable');
  await expect(unknown).toContainText('No priced usage · 0 of 10 events priced');
  await expect(unknown).toContainText('Missing prices: claude-future-model');
  const missing = leverage('Cards - missing model names');
  await expect(missing.locator('.val')).toHaveText('Unavailable');
  await expect(missing).toContainText('Missing model name: 10 events');
  await expect(missing).not.toContainText('Missing prices:');
  const zero = leverage('Cards - priced zero');
  await expect(zero.locator('.val')).toHaveText('~$0.00');
  await expect(zero.locator('.coverage')).toHaveCount(0);
});

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

test('real popover keeps oversized degraded content reachable', async ({ page }) => {
  await page.goto('/gallery.html?scroll-regression', { waitUntil: 'domcontentloaded' });

  const popover = page.locator('[data-testid="scroll-regression"] .pop');
  await expect(popover).toBeVisible();
  await expect(popover).toHaveCSS('overflow-y', 'auto');
  await expect(popover).toHaveCSS('max-height', '720px');

  const dimensions = await popover.evaluate((element) => ({
    clientHeight: element.clientHeight,
    scrollHeight: element.scrollHeight,
  }));
  expect(dimensions.scrollHeight).toBeGreaterThan(dimensions.clientHeight);

  const reachedEnd = await popover.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
    const lastChild = element.lastElementChild;
    if (!lastChild) return false;
    const viewport = element.getBoundingClientRect();
    const last = lastChild.getBoundingClientRect();
    return element.scrollTop > 0 && last.bottom <= viewport.bottom && last.bottom >= viewport.top;
  });
  expect(reachedEnd).toBe(true);
});

test('Cards marks every bar stale when one Codex window has reset', async ({ page }) => {
  await page.clock.setFixedTime(FIXED);
  await page.goto('/gallery.html?theme=light', { waitUntil: 'domcontentloaded' });

  const frame = page.locator('figure.frame').filter({
    has: page.getByText('Cards - Codex stale window', { exact: true }),
  });
  await expect(frame).toHaveCount(1);
  const codexCard = frame.locator('.pcard').filter({
    has: page.getByText('OpenAI', { exact: true }),
  });
  await expect(codexCard).toHaveCount(1);
  await expect(codexCard.locator('.brow')).toHaveCount(2);
  await expect(codexCard.locator('.sfb')).toHaveCount(2);
});
