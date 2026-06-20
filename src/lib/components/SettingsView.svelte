<script lang="ts">
  import { onMount } from 'svelte';
  import {
    getSettings,
    setSettings,
    setApiKey,
    hasApiKey,
    clearApiKey,
    getStatuslineStatus,
    setStatuslineWired,
  } from '$lib/ipc';
  import type { Settings, StatuslineWire } from '$lib/types/settings';

  let { onBack }: { onBack: () => void } = $props();

  let settings = $state<Settings | null>(null);
  let wire = $state<StatuslineWire | null>(null);
  let keyPresent = $state(false);
  let editingKey = $state(false);
  let keyInput = $state('');
  let status = $state<string | null>(null);
  let busy = $state(false);

  async function load() {
    try {
      settings = await getSettings();
    } catch (e) {
      status = `Couldn't load settings: ${e}`;
    }
    try {
      keyPresent = await hasApiKey('openai');
    } catch {
      keyPresent = false;
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
      editingKey = false;
      status = 'OpenAI key saved.';
      await load();
    } catch (e) {
      status = `Save failed: ${e}`;
    } finally {
      busy = false;
    }
  }

  async function removeKey() {
    busy = true;
    status = null;
    try {
      await clearApiKey('openai');
      status = 'OpenAI key removed.';
      await load();
    } catch (e) {
      status = `Remove failed: ${e}`;
    } finally {
      busy = false;
    }
  }

  async function toggle(provider: 'openai' | 'anthropic' | 'codex', value: boolean) {
    const current = settings;
    if (!current) return;
    const providers = { ...current.providers };
    if (provider === 'openai') providers.openai_enabled = value;
    else if (provider === 'anthropic') providers.anthropic_enabled = value;
    else providers.codex_enabled = value;
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
    <div class="connector">
      <div class="chd">
        <span
          class="dot"
          style:background={settings.providers.anthropic_enabled ? 'var(--ok)' : 'var(--warn)'}
        ></span>
        <span class="title">Anthropic / Claude</span>
      </div>

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

      <label class="toggle">
        <input
          type="checkbox"
          checked={settings.providers.anthropic_enabled}
          disabled={busy}
          onchange={(e) => toggle('anthropic', e.currentTarget.checked)}
        />
        <span>Anthropic OAuth polling</span>
      </label>
    </div>

    <div class="connector">
      <div class="chd">
        <span
          class="dot"
          style:background={keyPresent ? 'var(--ok)' : 'var(--warn)'}
        ></span>
        <span class="title">OpenAI</span>
      </div>

      <section>
        <div class="label">OpenAI Admin API key</div>
        <div class="hint">Stored in the OS keychain, never in a config file.</div>
        {#if keyPresent && !editingKey}
          <div class="hint">✓ A key is configured.</div>
          <div class="row">
            <button class="save" onclick={() => (editingKey = true)} disabled={busy}>Replace</button>
            <button class="save" onclick={removeKey} disabled={busy}>Remove</button>
          </div>
        {:else}
          <div class="row">
            <input
              type="password"
              placeholder="sk-admin-..."
              autocomplete="off"
              bind:value={keyInput}
              disabled={busy}
            />
            <button class="save" onclick={saveKey} disabled={busy}>Save</button>
            {#if keyPresent}
              <button
                class="save"
                onclick={() => {
                  editingKey = false;
                  keyInput = '';
                }}
                disabled={busy}>Cancel</button
              >
            {/if}
          </div>
        {/if}
      </section>

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
          checked={settings.providers.codex_enabled}
          disabled={busy}
          onchange={(e) => toggle('codex', e.currentTarget.checked)}
        />
        <span>Codex quota scanning</span>
      </label>
    </div>
  {:else}
    <div class="loading">Loading settings...</div>
  {/if}

  {#if status}
    <div class="status">{status}</div>
  {/if}
</div>

<style>
  .settings { padding: var(--sp-4) var(--sp-4) var(--sp-4); display: flex; flex-direction: column; gap: var(--sp-4); }
  .hd { display: flex; align-items: center; gap: var(--sp-2); }
  .back { background: none; border: none; color: var(--faint); cursor: pointer; font-size: var(--text-md); padding: 2px var(--sp-1); }
  .name { font-size: var(--text-lg); font-weight: 700; letter-spacing: -.01em; }
  .connector { border: 1.4px solid var(--tile-border); border-radius: 12px; background: var(--tile-bg);
    padding: var(--sp-3); display: flex; flex-direction: column; gap: var(--sp-3); }
  .chd { display: flex; align-items: center; gap: var(--sp-2); }
  .dot { width: var(--sp-2); height: var(--sp-2); border-radius: 50%; flex: none; }
  .title { font-size: var(--text-base); font-weight: 600; }
  section { display: flex; flex-direction: column; gap: 6px; }
  .label { font-size: var(--text-sm); font-weight: 600; }
  .hint { font-size: var(--text-xs); color: var(--faint); line-height: 1.35; }
  .row { display: flex; gap: var(--sp-2); margin-top: 2px; }
  input[type='password'] { flex: 1; min-width: 0; font-size: var(--text-sm); padding: 6px var(--sp-2); border-radius: 6px;
    border: 1px solid var(--hair); background: var(--paper); color: inherit; font-family: inherit; }
  .save { font-size: var(--text-sm); padding: 6px var(--sp-3); border-radius: 6px; border: 1px solid var(--hair);
    background: var(--paper); color: inherit; cursor: pointer; }
  .save:disabled { opacity: .5; cursor: default; }
  .toggle { display: flex; align-items: center; gap: var(--sp-2); font-size: var(--text-sm); cursor: pointer; }
  .toggle input { cursor: pointer; }
  .occupied { font-size: var(--text-xs); font-family: ui-monospace, monospace; color: var(--faint);
    word-break: break-all; padding: var(--sp-1) 0; }
  .loading { font-size: var(--text-sm); color: var(--faint); }
  .status { font-size: var(--text-xs); color: var(--faint); }
</style>
