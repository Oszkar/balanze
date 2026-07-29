import { test, expect } from '@playwright/test';
import { mkdir } from 'node:fs/promises';

// Capture path for the user guide's screenshots. Distinct from gallery.spec.ts
// on purpose: that spec captures `figure.frame` (caption included) as a
// visual-regression baseline, while these are read by a person and must not
// carry the gallery's own labels.
//
// We clip rather than screenshot `.pop` directly, because .caret is positioned
// at top:-7px and therefore lies outside .pop's bounding box - an element
// screenshot would cut it off.

const FIXED = new Date('2026-01-01T12:00:00Z');
const OUT = 'docs/src/assets/guide';

// The shared config's `Desktop Chrome` device is deviceScaleFactor 1. Guide
// assets are displayed at the popover's real 360px width, so capture at 2x to
// stay crisp on HiDPI screens. gallery.spec.ts deliberately keeps 1x - its
// committed baselines would all churn otherwise.
test.use({ deviceScaleFactor: 2 });

// Gallery figcaption label -> output filename. Every label must exist in
// src/lib/gallery/fixtures.ts; a rename there fails this spec loudly rather
// than silently shipping a stale image.
const SHOTS: Record<string, string> = {
  'Settings - configured': 'settings-configured',
  'Cards - two providers': 'details-two-providers',
  'Grid - overage billed': 'overage-billed',
  'Grid - two providers': 'compact-grid',
  'Grid - OpenAI connect CTA': 'openai-connect-cta',
  'Grid - cold start (quota loading)': 'state-cold-start',
  'Grid - Claude Code not detected': 'state-not-detected',
  'Grid - Codex stale window': 'state-stale-window',
  'Grid - OpenAI error': 'state-fetch-error',
};

test('capture guide screenshots', async ({ page }) => {
  await mkdir(OUT, { recursive: true });

  await page.clock.setFixedTime(FIXED);
  await page.goto('/gallery.html?theme=light', { waitUntil: 'domcontentloaded' });
  await page.locator('figure.frame').first().waitFor();
  await page.evaluate(() => document.fonts.ready);
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

  for (const [label, slug] of Object.entries(SHOTS)) {
    const frame = page.locator('figure.frame', { has: page.locator(`figcaption:text-is("${label}")`) });
    await expect(frame, `gallery state "${label}" not found in fixtures.ts`).toHaveCount(1);

    const pop = frame.locator('.pop');
    // The gallery renders every fixture on one long page (34 entries), so most
    // frames sit outside the initial viewport. page.screenshot's clip is
    // viewport-relative (not page-relative) unless fullPage is set, so an
    // off-screen frame's clip lands outside the rendered screenshot and
    // Playwright errors with "Clipped area is either empty or outside the
    // resulting image." Scroll the frame into view first so its bounding box
    // is always inside the captured viewport.
    await frame.scrollIntoViewIfNeeded();
    const frameBox = await frame.boundingBox();
    const popBox = await pop.boundingBox();
    if (!frameBox || !popBox) throw new Error(`no bounding box for "${label}"`);

    // Start 8px above .pop so the -7px caret is inside the capture, and end at
    // the frame's bottom edge. The caption sits above .pop, so this drops it.
    const top = popBox.y - 8;
    await page.screenshot({
      path: `${OUT}/${slug}.png`,
      // Raw page.screenshot leaves CSS animations running - unlike
      // toHaveScreenshot, which disables them by default (gallery.spec.ts relies
      // on that). Freezing Date does not freeze an animation timeline, so the
      // cold-start skeleton's infinite opacity pulse (GridView.svelte) and the
      // finite `rise` entrance animations would land at whatever phase they
      // happened to be in, making regenerated PNGs differ for no real reason.
      animations: 'disabled',
      clip: {
        x: popBox.x,
        y: top,
        width: popBox.width,
        height: frameBox.y + frameBox.height - top,
      },
    });
  }
});
