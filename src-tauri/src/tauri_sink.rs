//! Production sink for the `state_coordinator` actor: the side-effect
//! implementation the coordinator notifies after every snapshot merge. It
//! will emit `usage_updated` / `degraded_state` events to the Svelte UI
//! (AGENTS.md §4 #9 IPC contract) and repaint the tray icon / title with
//! the AGENTS.md §3.1 dedup discipline.
//!
//! This is currently a compile-only skeleton. It proves the
//! `state_coordinator::Sink` trait shape actually compiles inside the
//! `src-tauri` crate against a realistic `TauriSink` signature — so the UI
//! wiring doesn't later discover that the trait needs `&Snapshot` to be
//! `Send`, that an async sink is required, or that the `last_painted` field
//! set is wrong. The method bodies are left as `TODO:` markers; the live
//! Tauri calls (`app.emit`, `tray.set_icon`) are not implemented here yet.
//!
//! Per AGENTS.md §4 #7, this is the ONLY crate that may call Tauri tray
//! APIs (`tray.set_icon`, `tray.set_title`) when those bodies land. The
//! coordinator routes side effects through the sink; nothing else touches
//! OS tray state directly.

#![allow(dead_code)] // skeleton — fields wired when the UI lands

use state_coordinator::{Sink, Snapshot, Source};
use tauri::AppHandle;

/// Color bucket for the tray icon, mapped from the rolling-window usage %.
/// The bucketing thresholds + icon assets are TODO; this stand-in exists so
/// the `last_painted` dedup tuple has a concrete type to compare
/// (AGENTS.md §3.1: "Tray icon repaint: 30s cadence, deduped by
/// `(ColorBucket, title_text)`"). When the real color buckets land they may
/// live in their own module — this enum is the placeholder, not the final
/// home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorBucket {
    Green,
    Yellow,
    Orange,
    Red,
}

/// Production sink: emits Tauri events to the Svelte UI and repaints the
/// OS tray. Held inside the `state_coordinator` actor task; methods are
/// synchronous (the `Sink` trait requires sync, matching Tauri's sync
/// emit/tray APIs). `last_painted` carries the previous
/// `(ColorBucket, title_text)` tuple so a `Refresh` tick that doesn't
/// change the visible state can short-circuit without calling
/// `tray.set_icon` — see AGENTS.md §3.1.
pub(crate) struct TauriSink {
    app: AppHandle,
    last_painted: Option<(ColorBucket, String)>,
}

impl TauriSink {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self {
            app,
            last_painted: None,
        }
    }
}

impl Sink for TauriSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        // TODO:
        //   1. `self.app.emit("usage_updated", snapshot)` — sends the
        //      Snapshot DTO to the Svelte frontend per AGENTS.md §4 #9.
        //   2. Compute `(ColorBucket, title_text)` from `snapshot`'s
        //      rolling-window usage % and the configured thresholds.
        //   3. If that tuple differs from `self.last_painted`, call
        //      `tray.set_icon(...)` + `tray.set_title(...)` and update
        //      `self.last_painted`. Otherwise no-op (§3.1 dedup).
        //   4. Tray handle: `self.app.tray_by_id("main")` — keep the same
        //      id as `setup_tray` in `lib.rs`.
        //
        // Touch the fields so `#[allow(dead_code)]` is the only suppressant
        // we need — no `_` underscored locals, no extra `#[allow]` noise.
        let _ = &self.app;
        let _ = &self.last_painted;
        let _ = snapshot.fetched_at;
    }

    fn on_degraded(&mut self, source: Source, error: &str) {
        // TODO:
        //   1. `self.app.emit("degraded_state", DegradedPayload { source, error })`
        //      — surfaces the failure to the UI's warning indicator.
        //   2. Tray-side: a degraded source SHOULD flip the icon to the
        //      warning-dot variant regardless of bucket (so the user sees
        //      something is off even when usage is otherwise green).
        //      Decide whether degraded-state lives in `ColorBucket` as a
        //      variant or as a parallel boolean — the answer depends on
        //      how the icon asset set is structured.
        let _ = &self.app;
        let _ = source;
        let _ = error;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-only assertion: the trait bounds Sink requires (Send + 'static)
    // are satisfied by TauriSink. If a future change to Sink (or to
    // AppHandle's auto-traits) breaks this, the build fails here rather than
    // at the UI wiring site.
    #[allow(dead_code)]
    fn assert_sink_bounds<S: Sink>() {}

    #[allow(dead_code)]
    fn _seam_check() {
        assert_sink_bounds::<TauriSink>();
    }
}
