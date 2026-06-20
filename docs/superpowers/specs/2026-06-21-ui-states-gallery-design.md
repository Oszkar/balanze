# UI States Gallery - Design

**Date:** 2026-06-21
**Scope:** v0.4.0 (separate PR from the popover polish)
**Status:** approved

## Problem

The popover's error and empty states (cold-start loading, connect CTA, fetch errors, dismissed column, stale windows) are hard to reach by running the app - they depend on provider failures, cold caches, and timing that you cannot summon on demand. There is no single place to see every screen at once. The original static wireframes (a claude.ai/design HTML mock) served that role but have drifted from the real tokens and components and should be retired.

## Goal

A development-only canvas that renders every popover screen and cell state side by side, using the real Svelte components and the real `theme.css` design tokens - the living successor to the static wireframes, and a source of portfolio screenshots.

## Approach

A `/gallery` SvelteKit route (the app is already SPA, `ssr = false`, so a route is just another page). It renders a labeled grid of frames; each frame is the real sub-view (`GridView` / `CardsView` / `SettingsView`) fed a hand-built fixture `Snapshot`, wrapped in the popover's `.pop` chrome at the fixed 360px width. A light/dark toggle flips a scoped palette so both themes are viewable regardless of OS preference.

Rejected alternatives: Storybook/Histoire (heavyweight dev dep + parallel build, against YAGNI for a solo app); another static HTML mock (exactly what we are retiring - it drifts from real tokens).

### Why sub-views, not the full `Popover`

`Popover` owns `view` (grid/cards) as internal `$state`, so it cannot be forced to a given view from outside. Rendering the sub-views directly lets the gallery show grid and cards states deterministically and side by side, each fed pure props. `GridView` and `CardsView` make no IPC calls, so they render anywhere. The frame composes them exactly as `Popover` does (grid: `GridView` + `BurnIndicator` + `LeverageBox`; cards: `CardsView` + `LeverageBox`), and includes the real `Header` with a local `view` seeded from the descriptor - so the segmented picker is a live, safe grid/cards toggle per frame. All interactive callbacks (refresh, settings, dismiss) are no-ops.

### IPC

`SettingsView` is the only sub-view that calls IPC (`get_settings`, `has_api_key`, `get_statusline_status`). The gallery installs `mockIPC` (from `@tauri-apps/api/mocks`) to return canned values so the one Settings frame renders in a plain browser with no Tauri host. It is installed during the route's component init (not `onMount`): a child's `onMount` runs before the parent's, so an `onMount` setup would let `SettingsView`'s first `get_settings` race ahead of the mock and hit the absent runtime. `mockIPC` replaces the whole `invoke` transport, so writes (`set_settings`, `set_api_key`, `clear_api_key`, `set_statusline_wired`, ...) are intercepted too - each is an explicit, logged no-op, so clicking Remove/Save/toggle in the Settings frame can never touch the real keychain or settings file. Mock setup is gated behind `import.meta.env.DEV`.

### Theming

`theme.css` exposes the dark palette only through `@media (prefers-color-scheme: dark)`, with no class hook. To make the toggle deterministic (force light or dark independent of OS), the gallery route mirrors both palettes in a scoped `.canvas.light` / `.canvas.dark` override. This duplicates ~22 token values in one dev-only file; it is deliberately kept out of the shared `theme.css` so the shipping app's theming is untouched. The mirror carries a "keep in sync with theme.css" comment.

## Files

- `src/lib/gallery/fixtures.ts` - a `baseSnapshot()` factory plus named per-state overrides, and a `GALLERY_STATES` array of frame descriptors (`{ label, view, openaiEnabled, snapshot, degraded }`). Also `DEMO_SETTINGS` / `DEMO_STATUSLINE` for the mock.
- `src/lib/gallery/GalleryFrame.svelte` - one frame: a caption + the `.pop`-chromed sub-view composition.
- `src/routes/gallery/+page.svelte` - the canvas: dev gate, `mockIPC` setup, light/dark toggle, the frame grid.

## States enumerated

Grid: two-provider data, cold-start loading, OpenAI connect CTA, OpenAI error, single-provider (Anthropic only), Codex stale window, Anthropic statusline-fallback, overage billed. Cards: two-provider data, single-provider, Codex stale, OpenAI error. Settings: configured.

## Out of scope / non-goals

- Not shipped to users (dev-only route; production renders a one-line notice).
- No new IPC commands, no schema changes, no secrets, no new runtime dependency (`mockIPC` ships with the existing `@tauri-apps/api`).
- The honesty invariant is unaffected - the gallery only renders existing components with fixture data.

## Follow-up

Once merged, retire the claude.ai/design wireframes as the canonical reference (point the README/design notes at this route).
