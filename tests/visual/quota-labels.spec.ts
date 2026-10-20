import { test, expect } from '@playwright/test';

for (const theme of ['light', 'dark']) {
  test(`quota labels preserve window identity - ${theme}`, async ({ page }, testInfo) => {
    await page.clock.setFixedTime(new Date('2026-01-01T12:00:00Z'));
    await page.goto(`/gallery.html?label-regression&theme=${theme}`);
    const frame = (label: string) => page.locator('figure.frame').filter({ has: page.getByText(label, { exact: true }) });
    await expect(frame('Grid - model limit')).toContainText('7d-opus 91%');
    for (const source of ['OAuth', 'statusline']) {
      const cards = frame(`Cards - ${source} labels`);
      await expect(cards).toContainText('5-hour');
      await expect(cards).toContainText('7-day Opus');
      await expect(cards).not.toContainText('Current 5-hour session');
      await expect(cards).toContainText('Codex window');
    }
    for (const view of ['Grid', 'Cards']) {
      await expect(frame(`${view} - unknown limit`)).toContainText('Monthly Phoenix');
    }
    await page.evaluate(() => document.fonts.ready);
    for (const label of ['Grid - model limit', 'Cards - OAuth labels', 'Grid - unknown limit', 'Cards - unknown limit']) {
      const element = frame(label);
      const overflow = await element.evaluate((el) => el.scrollWidth > el.clientWidth);
      expect(overflow, label).toBe(false);
      await testInfo.attach(label, { body: await element.screenshot(), contentType: 'image/png' });
    }
    await page.screenshot({ path: testInfo.outputPath('quota-labels.png'), fullPage: true });
  });
}
