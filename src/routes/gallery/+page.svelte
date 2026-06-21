<script lang="ts">
  import { onDestroy } from 'svelte';
  import { mockIPC, clearMocks } from '@tauri-apps/api/mocks';
  import GalleryFrame from '$lib/gallery/GalleryFrame.svelte';
  import { GALLERY_STATES, DEMO_SETTINGS, DEMO_STATUSLINE } from '$lib/gallery/fixtures';

  // Dev-only surface: the production bundle renders a notice instead. `mockIPC`
  // ships with @tauri-apps/api (no new dependency) and lets the one Settings
  // frame resolve its IPC reads in a plain browser with no Tauri host.
  const dev = import.meta.env.DEV;
  let theme = $state<'light' | 'dark'>('light');

  // Install the stub during component init, NOT in onMount: a child's onMount
  // (SettingsView's `load`) runs before the parent's, so an onMount setup would
  // race and the first `get_settings` would hit the absent Tauri runtime
  // (window.__TAURI_INTERNALS__ undefined). Running here puts the mock in place
  // before any child mounts. Browser-only (ssr=false), guarded for safety.
  if (dev && typeof window !== 'undefined') {
    mockIPC((cmd) => {
      switch (cmd) {
        // Reads -> canned data so the Settings frame renders.
        case 'get_settings':
          return DEMO_SETTINGS;
        case 'has_api_key':
          return true;
        case 'get_statusline_status':
          return DEMO_STATUSLINE;
        // Writes -> swallowed. mockIPC replaces the whole invoke transport, so a
        // mutating command (Remove key, Save, toggle a provider, Wire) never
        // reaches the real keychain or settings file - even if this route is
        // opened inside the Tauri app. Each is an explicit, logged no-op so the
        // safety is intentional, not an accident of the default branch.
        case 'set_settings':
        case 'set_api_key':
        case 'clear_api_key':
        case 'set_statusline_wired':
        case 'refresh_now':
        case 'hide_window':
        case 'resize_popover':
          console.debug(`[gallery] mocked no-op write: ${cmd}`);
          return undefined;
        default:
          console.debug(`[gallery] unhandled mocked command: ${cmd}`);
          return undefined;
      }
    });
    onDestroy(clearMocks);
  }
</script>

{#if dev}
  <div class="canvas" class:dark={theme === 'dark'} class:light={theme === 'light'}>
    <header class="bar">
      <h1>Balanze - states gallery</h1>
      <button type="button" onclick={() => (theme = theme === 'light' ? 'dark' : 'light')}>
        Switch to {theme === 'light' ? 'dark' : 'light'}
      </button>
    </header>
    <div class="grid">
      {#each GALLERY_STATES as s (s.label)}
        <GalleryFrame
          label={s.label}
          view={s.view}
          snapshot={s.snapshot}
          degraded={s.degraded}
          openaiEnabled={s.openaiEnabled}
        />
      {/each}
    </div>
  </div>
{:else}
  <p class="prod">The states gallery is a development-only surface.</p>
{/if}

<style>
  .canvas { min-height: 100vh; padding: 40px 32px 56px;
    font-family: 'Inter', system-ui, -apple-system, 'Segoe UI', sans-serif; color: var(--ink); }
  .bar { display: flex; align-items: center; justify-content: space-between; margin-bottom: 32px; }
  h1 { font-size: 16px; font-weight: 700; margin: 0; }
  .bar button { font-size: 12px; font-weight: 600; padding: 6px 14px; border-radius: 8px;
    border: 1px solid var(--seg-border); background: var(--seg-on); color: var(--seg-on-text); cursor: pointer; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, 360px); gap: 44px 28px; align-items: start; }
  .prod { font-family: system-ui, sans-serif; padding: 40px; color: #888; }

  /* Force a theme independent of the OS `prefers-color-scheme`. theme.css only
     exposes the dark palette via a media query, so both palettes are mirrored
     here, scoped to the canvas. KEEP IN SYNC WITH src/lib/theme.css. Dev-only -
     deliberately not added to the shared theme so the shipping app is untouched. */
  .canvas.light {
    --paper: #ffffff; --ink: #1f2227; --ink2: #5a5f68; --faint: #9aa0aa;
    --ok: #3f8f5f; --warn: #cf8a2a; --bad: #c0493a; --real: #3667a6;
    --hair: #e6e2db; --tile-bg: #fbfaf7; --tile-border: #e8e3db;
    --track: #ece8e1; --hatch: rgba(0, 0, 0, .09);
    --seg-on: #1f2227; --seg-on-text: #fff; --seg-border: #d9d4cc;
    --lev-border: #d9d4cc; --lev-bg: #faf8f4;
    --tip-bg: #26292e; --tip-ink: #f2f3f5; --tip-faint: #aab2bd;
    --shadow: 0 16px 44px rgba(40, 36, 28, .18);
    background: #efede8;
  }
  .canvas.dark {
    --paper: #171a20; --ink: #e9ecf1; --ink2: #aab2bd; --faint: #6f7886;
    --ok: #57c98a; --warn: #e6a24a; --bad: #e3685a; --real: #6ea3e6;
    --hair: rgba(255, 255, 255, .10); --tile-bg: rgba(255, 255, 255, .035); --tile-border: rgba(255, 255, 255, .08);
    --track: rgba(255, 255, 255, .10); --hatch: rgba(255, 255, 255, .10);
    --seg-on: #e9ecf1; --seg-on-text: #171a20; --seg-border: rgba(255, 255, 255, .22);
    --lev-border: rgba(255, 255, 255, .18); --lev-bg: rgba(255, 255, 255, .02);
    --tip-bg: #0d0f13; --tip-ink: #e9ecf1; --tip-faint: #8a93a0;
    --shadow: 0 18px 50px rgba(0, 0, 0, .55);
    background: #0b0d11;
  }
</style>
