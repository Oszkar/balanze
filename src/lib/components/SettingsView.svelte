<script lang="ts">
  import { onMount } from 'svelte';
  import {
    getSettings,
    setSettings,
    setApiKey,
    validateApiKey,
    hasApiKey,
    clearApiKey,
    getStatuslineStatus,
    setStatuslineWired,
    openExternal,
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
  let canSaveAnyway = $state(false);

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

  // Persist a validated (or save-anyway) key. Handles its own errors so the
  // caller stays focused on the validate flow. The key goes straight to the
  // keychain backend-side; we never keep it.
  async function persistKey(k: string) {
    try {
      await setApiKey('openai', k);
      keyInput = '';
      editingKey = false;
      canSaveAnyway = false;
      status = 'OpenAI key saved.';
      await load();
    } catch (e) {
      status = `Save failed: ${e}`;
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
    canSaveAnyway = false;
    try {
      // Probe the key before storing it, so a wrong key is caught now instead
      // of a poll interval later.
      const v = await validateApiKey('openai', k);
      if (v.ok) {
        await persistKey(k);
      } else if (v.retryable) {
        // Transient (network / rate limit): the key may be fine. Offer to store
        // it anyway and let the poller retry.
        status = v.message ?? 'Could not verify the key right now.';
        canSaveAnyway = true;
      } else {
        // Definitive (invalid key / wrong scope): do not store it.
        status = v.message ?? 'OpenAI rejected that key.';
      }
    } catch (e) {
      status = `Couldn't check the key: ${e}`;
    } finally {
      busy = false;
    }
  }

  async function saveAnyway() {
    const k = keyInput.trim();
    if (!k) return;
    busy = true;
    status = null;
    try {
      await persistKey(k);
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
    <button class="back" type="button" aria-label="Back" title="Back" onclick={onBack}><span aria-hidden="true">←</span></button>
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
          <div class="hint">
            Needs an <strong>admin</strong> key (<code>sk-admin-...</code>). Project and
            service-account keys can't read organization billing.
            <button
              type="button"
              class="link"
              onclick={() =>
                openExternal('https://platform.openai.com/settings/organization/admin-keys')}
              >Get an admin key</button
            >
          </div>
          <div class="row">
            <input
              type="password"
              aria-label="OpenAI Admin API key"
              placeholder="sk-admin-..."
              autocomplete="off"
              bind:value={keyInput}
              oninput={() => (canSaveAnyway = false)}
              disabled={busy}
            />
            <button class="save" onclick={saveKey} disabled={busy}>Save</button>
            {#if keyPresent}
              <button
                class="save"
                onclick={() => {
                  editingKey = false;
                  keyInput = '';
                  canSaveAnyway = false;
                }}
                disabled={busy}>Cancel</button
              >
            {/if}
          </div>
          {#if canSaveAnyway}
            <div class="row">
              <button class="save" onclick={saveAnyway} disabled={busy}>Save anyway</button>
            </div>
          {/if}
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
    <div class="status" role="status" aria-live="polite">{status}</div>
  {/if}
</div>

<style>
  .settings { padding: var(--sp-4) var(--sp-4) var(--sp-4); display: flex; flex-direction: column; gap: var(--sp-4); }
  .hd { display: flex; align-items: center; gap: var(--sp-2); }
  .back { background: none; border: none; color: var(--faint); cursor: pointer; font-size: var(--text-md); padding: 2px var(--sp-1); border-radius: 4px; }
  .back:hover { color: var(--ink); }
  .back:focus-visible { outline: 2px solid var(--ink2); outline-offset: 2px; }
  .name { font-size: var(--text-lg); font-weight: 700; letter-spacing: -.01em; }
  .connector { border-radius: 12px; background: var(--tile-face); box-shadow: var(--tile-elev);
    padding: var(--sp-3); display: flex; flex-direction: column; gap: var(--sp-3); }
  .chd { display: flex; align-items: center; gap: var(--sp-2); }
  .dot { width: var(--sp-2); height: var(--sp-2); border-radius: 50%; flex: none; }
  .title { font-size: var(--text-base); font-weight: 600; }
  section { display: flex; flex-direction: column; gap: 6px; }
  .label { font-size: var(--text-sm); font-weight: 600; }
  .hint { font-size: var(--text-xs); color: var(--faint); line-height: 1.35; }
  .link { background: none; border: none; padding: 0; font: inherit; color: var(--ink2);
    text-decoration: underline; cursor: pointer; }
  .link:hover { color: var(--ink); }
  .link:focus-visible { outline: 2px solid var(--ink2); outline-offset: 2px; border-radius: 2px; }
  code { font-family: 'JetBrains Mono', ui-monospace, monospace; font-size: .92em; }
  .row { display: flex; gap: var(--sp-2); margin-top: 2px; }
  input[type='password'] { flex: 1; min-width: 0; font-size: var(--text-sm); padding: 6px var(--sp-2); border-radius: 6px;
    border: 1px solid var(--hair); background: var(--paper); color: inherit; font-family: inherit; }
  input[type='password']:focus-visible { outline: 2px solid var(--ink2); outline-offset: 1px; }
  .save { font-size: var(--text-sm); padding: 6px var(--sp-3); border-radius: 6px; border: 1px solid var(--hair);
    background: var(--paper); color: inherit; cursor: pointer; }
  .save:hover:not(:disabled) { border-color: var(--ink2); }
  .save:focus-visible { outline: 2px solid var(--ink2); outline-offset: 2px; }
  .save:disabled { opacity: .5; cursor: default; }
  .toggle { display: flex; align-items: center; gap: var(--sp-2); font-size: var(--text-sm); cursor: pointer; }
  .toggle input { cursor: pointer; }
  .occupied { font-size: var(--text-xs); font-family: 'JetBrains Mono', ui-monospace, monospace; color: var(--faint);
    word-break: break-all; padding: var(--sp-1) 0; }
  .loading { font-size: var(--text-sm); color: var(--faint); }
  .status { font-size: var(--text-xs); color: var(--faint); }
</style>
