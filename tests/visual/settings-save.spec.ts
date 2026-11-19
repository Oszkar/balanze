import { test, expect, type Page } from '@playwright/test';
import type { Settings, SettingsPatch } from '../../src/lib/types/settings';

// Exercise the shipping component through the gallery's isolated IPC transport.
// Neither successful nor rejected writes touch real settings or credentials.
async function configureSave(page: Page, outcome: 'reject' | 'committed-error' | 'success' | 'read-error' | 'normalized') {
  await page.evaluate(async (outcome) => {
    const internals = (window as unknown as {
      __TAURI_INTERNALS__: { invoke: (cmd: string, args?: { patch: SettingsPatch }) => Promise<unknown> };
    }).__TAURI_INTERNALS__;
    const original = internals.invoke;
    let persisted = structuredClone(await original('get_settings')) as Settings;
    let failed = false;
    internals.invoke = async (cmd, args) => {
      if (cmd === 'get_settings') {
        if (outcome === 'read-error' && failed) throw new Error('read unavailable');
        return structuredClone(persisted);
      }
      if (cmd === 'set_settings') {
        const patch = args!.patch;
        if (outcome === 'success' || outcome === 'committed-error') {
          persisted = { ...persisted, ...patch,
            providers: { ...persisted.providers, ...patch.providers },
            tray: { ...persisted.tray, ...patch.tray } };
        }
        if (outcome === 'success' || outcome === 'normalized') return structuredClone(persisted);
        failed = true;
        throw new Error(outcome === 'committed-error' ? 'live apply failed' : 'save denied');
      }
      return original(cmd, args);
    };
  }, outcome);
}

const checkboxNames = [
  'Show percentages in the menu bar', 'Anthropic OAuth polling',
  'OpenAI usage polling', 'Codex quota scanning',
];

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => Object.defineProperty(navigator, 'platform', { get: () => 'MacIntel' }));
  await page.goto('/gallery.html', { waitUntil: 'domcontentloaded' });
  await expect(page.getByRole('combobox', { name: 'Tray provider display' })).toHaveValue('separate');
});

for (const name of checkboxNames) {
  test(`${name} restores persisted state after a rejected save`, async ({ page }) => {
    await configureSave(page, 'reject');
    const control = page.getByRole('checkbox', { name, exact: true });
    const before = await control.isChecked();
    await control.click();
    await expect(page.getByRole('figure', { name: 'Settings - configured', exact: true }).getByRole('status')).toContainText('save denied');
    await expect(control).toBeEnabled();
    await expect(control).toBeChecked({ checked: before });
  });
}

for (const outcome of ['reject', 'committed-error', 'success', 'normalized'] as const) {
  test(`tray mode reconciles ${outcome}`, async ({ page }) => {
    await configureSave(page, outcome);
    const control = page.getByRole('combobox', { name: 'Tray provider display' });
    await control.selectOption('combined');
    if (outcome === 'reject' || outcome === 'committed-error') {
      await expect(page.getByRole('figure', { name: 'Settings - configured', exact: true }).getByRole('status')).toContainText(outcome === 'reject' ? 'save denied' : 'live apply failed');
    }
    await expect(control).toBeEnabled();
    await expect(control).toHaveValue(outcome === 'reject' || outcome === 'normalized' ? 'separate' : 'combined');
  });
}

test('checkbox retains a committed preference when live application fails', async ({ page }) => {
  await configureSave(page, 'committed-error');
  const control = page.getByRole('checkbox', { name: checkboxNames[0] });
  await control.check();
  await expect(page.getByRole('figure', { name: 'Settings - configured', exact: true }).getByRole('status')).toContainText('live apply failed');
  await expect(control).toBeEnabled();
  await expect(control).toBeChecked();
});

test('failed reconciliation preserves both errors and offers recovery', async ({ page }) => {
  await configureSave(page, 'read-error');
  await page.getByRole('checkbox', { name: checkboxNames[0] }).click();
  await expect(page.getByRole('figure', { name: 'Settings - configured', exact: true }).getByRole('status')).toContainText('save denied');
  await expect(page.getByRole('figure', { name: 'Settings - configured', exact: true }).getByRole('status')).toContainText('read unavailable');
  await expect(page.getByRole('combobox', { name: 'Tray provider display' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Reload settings' })).toBeEnabled();
  // Restore reads without relying on the failing interceptor's private state.
  await page.evaluate(() => {
    const internals = (window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__;
    const original = internals.invoke;
    internals.invoke = async (cmd, args) => cmd === 'get_settings'
      ? { version: 2, providers: { anthropic_enabled: true, openai_enabled: true, codex_enabled: true }, tray: { mode: 'separate', show_percentages: false }, oauth_poll_interval_secs: 300, seen_welcome: true }
      : original(cmd, args);
  });
  await page.getByRole('button', { name: 'Reload settings' }).click();
  await expect(page.getByRole('combobox', { name: 'Tray provider display' })).toHaveValue('separate');
  await expect(page.getByRole('figure', { name: 'Settings - configured', exact: true }).getByRole('status')).toHaveCount(0);
});
