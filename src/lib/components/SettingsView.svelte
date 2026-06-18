<script lang="ts">
  import { onMount } from 'svelte';
  import {
    getSettings,
    setSettings,
    setApiKey,
    getStatuslineStatus,
    setStatuslineWired,
  } from '$lib/ipc';
  import type { Settings, StatuslineWire } from '$lib/types/settings';

  let { onBack }: { onBack: () => void } = $props();

  let settings = $state<Settings | null>(null);
  let wire = $state<StatuslineWire | null>(null);
  let keyInput = $state('');
  let status = $state<string | null>(null);
  let busy = $state(false);

  async function load() {
    try {
      settings = await getSettings();
    } catch (e) {
      status = `Couldn't load settings: ${e}`;
    }
    // statusLine status is independent - a read failure here shouldn't block
    // the rest of the panel, so just leave the section hidden.
    try {
      wire = await getStatuslineStatus();
    } catch {
      wire = null;
    }
  }

  onMount(load);

  async function setWire(wired: boolean) {
    busy = true;
    status = null;
    try {
      await setStatuslineWired(wired);
      wire = await getStatuslineStatus();
      status = wired
        ? 'statusLine wired. Restart Claude Code to see it.'
        : 'statusLine unwired.';
    } catch (e) {
      // e.g. the no-clobber refusal when another command owns the stanza.
      status = `${e}`;
    } finally {
      busy = false;
    }
  }

  async function saveKey() {
    const k = keyInput.trim();
    if (!k) {
      status = 'Enter a key first.';
      return;
    }
    busy = true;
    status = null;
    try {
      // The key goes straight to the keychain backend-side; we never keep it.
      await setApiKey('openai', k);
      keyInput = '';
      status = 'OpenAI key saved.';
      await load();
    } catch (e) {
      status = `Save failed: ${e}`;
    } finally {
      busy = false;
    }
  }

  async function toggle(provider: 'openai' | 'anthropic', value: boolean) {
    const current = settings;
    if (!current) return;
    const providers = { ...current.providers };
    if (provider === 'openai') providers.openai_enabled = value;
    else providers.anthropic_enabled = value;
    const next: Settings = { ...current, providers };
    busy = true;
    status = null;
    try {
      await setSettings(next);
      settings = next;
    } catch (e) {
      status = `Save failed: ${e}`;
      // We never optimistically mutated `settings`, so there's nothing to
      // reload - just reassign to the persisted value so the checkbox snaps
      // back from its clicked state. Avoids masking this error with a reload
      // failure and avoids an unnecessary round-trip.
      settings = { ...current };
    } finally {
      busy = false;
    }
  }
</script>

<div class="settings">
  <div class="hd">
    <button class="back" title="Back" onclick={onBack}>←</button>
    <div class="name">Settings</div>
  </div>

  {#if settings}
    <section>
      <div class="label">OpenAI Admin API key</div>
      <div class="hint">
        Stored in the OS keychain, never in a config file. {settings.providers.openai_enabled
          ? 'A key is configured.'
          : 'No key configured yet.'}
      </div>
      <div class="row">
        <input
          type="password"
          placeholder="sk-admin-..."
          autocomplete="off"
          bind:value={keyInput}
          disabled={busy}
        />
        <button class="save" onclick={saveKey} disabled={busy}>Save</button>
      </div>
    </section>

    <section>
      <div class="label">Providers</div>
      <label class="toggle">
        <input
          type="checkbox"
          checked={settings.providers.openai_enabled}
          disabled={busy}
          onchange={(e) => toggle('openai', e.currentTarget.checked)}
        />
        <span>OpenAI usage polling</span>
      </label>
      <label class="toggle">
        <input
          type="checkbox"
          checked={settings.providers.anthropic_enabled}
          disabled={busy}
          onchange={(e) => toggle('anthropic', e.currentTarget.checked)}
        />
        <span>Anthropic OAuth polling</span>
      </label>
    </section>

    {#if wire}
      <section>
        <div class="label">Claude Code statusLine</div>
        {#if wire.status === 'wired'}
          <div class="hint">✓ Wired to Balanze. Restart Claude Code to apply changes.</div>
          <div class="row">
            <button class="save" onclick={() => setWire(false)} disabled={busy}>Unwire</button>
          </div>
        {:else if wire.status === 'unwired'}
          <div class="hint">Show live 5h / 7d quota directly in Claude Code's status line.</div>
          <div class="row">
            <button class="save" onclick={() => setWire(true)} disabled={busy}>Wire</button>
          </div>
        {:else}
          <div class="hint">Set to another command - Balanze won't overwrite it:</div>
          <div class="occupied">{wire.command}</div>
        {/if}
      </section>
    {/if}
  {:else}
    <div class="loading">Loading settings...</div>
  {/if}

  {#if status}
    <div class="status">{status}</div>
  {/if}
</div>

<style>
  .settings { padding: 15px 16px 16px; display: flex; flex-direction: column; gap: 16px; }
  .hd { display: flex; align-items: center; gap: 8px; }
  .back { background: none; border: none; color: var(--faint); cursor: pointer; font-size: 16px; padding: 2px 4px; }
  .name { font-size: 18px; font-weight: 700; letter-spacing: -.01em; }
  section { display: flex; flex-direction: column; gap: 6px; }
  .label { font-size: 12px; font-weight: 600; }
  .hint { font-size: 11px; color: var(--faint); line-height: 1.35; }
  .row { display: flex; gap: 8px; margin-top: 2px; }
  input[type='password'] { flex: 1; min-width: 0; font-size: 12px; padding: 6px 8px; border-radius: 6px;
    border: 1px solid var(--hair); background: var(--paper); color: inherit; font-family: inherit; }
  .save { font-size: 12px; padding: 6px 12px; border-radius: 6px; border: 1px solid var(--hair);
    background: var(--paper); color: inherit; cursor: pointer; }
  .save:disabled { opacity: .5; cursor: default; }
  .toggle { display: flex; align-items: center; gap: 8px; font-size: 12px; cursor: pointer; }
  .toggle input { cursor: pointer; }
  .occupied { font-size: 11px; font-family: ui-monospace, monospace; color: var(--faint);
    word-break: break-all; padding: 4px 0; }
  .loading { font-size: 12px; color: var(--faint); }
  .status { font-size: 11px; color: var(--faint); }
</style>
