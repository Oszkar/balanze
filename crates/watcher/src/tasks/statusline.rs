//! Statusline notify task. Watches the Balanze data directory for changes to
//! `statusline.snapshot.json` (written by `balanze-cli statusline`), debounces
//! bursts for 100ms, then re-reads and emits the snapshot on each batch.
//!
//! The watch is non-recursive on the data directory — only direct children
//! generate events. On notify init failure the task returns
//! `Err(WatcherError::NotifyExhausted { affected: Source::ClaudeStatusline })`.
//! If the file does not exist (`FileIoError::FileMissing`) no event is emitted;
//! this is the normal state for users who haven't wired the statusLine yet.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use claude_statusline::{read_snapshot, FileIoError};
use notify::{RecursiveMode, Watcher as _};
use state_coordinator::{Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::errors::WatcherError;

/// Debounce window for statusline file changes — shorter than the JSONL
/// debounce (300ms) because the statusline file is a single small JSON blob
/// written once per `balanze-cli statusline` invocation, not a stream of
/// many small appends.
const DEBOUNCE: Duration = Duration::from_millis(100);

// MIRRORS balanze_cli::statusline_snapshot_path — see
// TODO(v0.2-followup): extract into a shared `paths` helper (either in
// `settings` or a small new `balanze_paths` crate) so CLI and watcher
// resolve the same path via one code path.
fn statusline_snapshot_path() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("BALANZE_DATA_DIR_OVERRIDE") {
        return Some(PathBuf::from(env_path).join("statusline.snapshot.json"));
    }
    directories::ProjectDirs::from("me", "oszkar", "Balanze")
        .map(|d| d.data_dir().join("statusline.snapshot.json"))
}

/// Spawn the statusline notify task and return its `JoinHandle`.
///
/// The task:
/// 1. Resolves `<data_dir>/statusline.snapshot.json`. If the data dir can't
///    be resolved, logs at `warn!` and exits `Ok(())`.
/// 2. Watches the parent directory (non-recursive) so any write to
///    `statusline.snapshot.json` wakes the debounce loop.
/// 3. Emits an initial read attempt on startup (covers an existing file
///    from a prior `balanze-cli statusline` run).
/// 4. On each debounced event, re-reads and emits.
///
/// `FileMissing` is not emitted — it's the expected state for users who
/// haven't wired `statusLine` in their Claude Code settings yet.
pub(crate) fn spawn(coord: StateCoordinatorHandle) -> JoinHandle<Result<(), WatcherError>> {
    tokio::spawn(async move {
        let snapshot_path = match statusline_snapshot_path() {
            Some(p) => p,
            None => {
                tracing::warn!("watcher/statusline: cannot resolve data dir; task exits clean");
                return Ok(());
            }
        };

        let watch_dir = match snapshot_path.parent() {
            Some(p) => p.to_path_buf(),
            None => {
                tracing::warn!("watcher/statusline: snapshot path has no parent; task exits clean");
                return Ok(());
            }
        };

        // The data directory may not yet exist (user hasn't run
        // `balanze-cli statusline` even once). Create it up front so the
        // notify watch registers cleanly; it's Balanze's own data dir, so
        // creating it ahead of the first producer write has no side-effects
        // beyond an empty directory on disk.
        //
        // Use `tokio::fs::create_dir_all` (async) rather than `std::fs::*`
        // so this startup I/O doesn't block a tokio worker — consistent
        // with the `spawn_blocking` discipline used elsewhere in the
        // watcher for sync FS work. On a slow / remote filesystem this
        // can actually take a measurable moment.
        if let Err(e) = tokio::fs::create_dir_all(&watch_dir).await {
            // `Io(e)` is the right variant — this is a plain filesystem
            // failure (permissions, read-only FS, parent missing on a
            // weird mount), NOT kernel notify resource exhaustion.
            // `NotifyExhausted` would mislead the supervisor's fallback
            // policy into the wrong direction.
            tracing::error!(
                "watcher/statusline: failed to create data dir {} ({e})",
                watch_dir.display()
            );
            return Err(WatcherError::Io(e));
        }

        let signal = Arc::new(Notify::new());
        let signal_cb = signal.clone();
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Err(e) = &res {
                    tracing::warn!("watcher/statusline: notify error: {e}");
                }
                signal_cb.notify_one();
            }) {
                Ok(w) => w,
                Err(e) => {
                    tracing::error!(
                        "watcher/statusline: notify init failed ({e}); reporting NotifyExhausted"
                    );
                    return Err(WatcherError::NotifyExhausted {
                        affected: Source::ClaudeStatusline,
                    });
                }
            };

        // Non-recursive: we only care about the data dir's direct children.
        // `create_dir_all` above guarantees the dir exists, so a watch
        // failure here is a real error (permissions, exhaustion) — treat
        // it the same way as the JSONL task (consistency).
        if let Err(e) = watcher.watch(&watch_dir, RecursiveMode::NonRecursive) {
            tracing::error!(
                "watcher/statusline: failed to watch {} ({e}); reporting NotifyExhausted",
                watch_dir.display()
            );
            return Err(WatcherError::NotifyExhausted {
                affected: Source::ClaudeStatusline,
            });
        }

        // Initial read on task startup — covers the file already existing.
        emit_statusline_snapshot(&coord, &snapshot_path).await;

        loop {
            signal.notified().await;
            tokio::time::sleep(DEBOUNCE).await;
            emit_statusline_snapshot(&coord, &snapshot_path).await;
        }
    })
}

/// Read the statusline snapshot from disk (sync) and emit an update to the
/// coordinator. `FileMissing` is silently swallowed — it means the user hasn't
/// wired statusLine yet and is not an error state.
async fn emit_statusline_snapshot(coord: &StateCoordinatorHandle, path: &std::path::Path) {
    let path_owned = path.to_path_buf();
    let result = tokio::task::spawn_blocking(move || read_snapshot(&path_owned)).await;

    let read_result = match result {
        Ok(r) => r,
        Err(join_err) => {
            tracing::error!("watcher/statusline: read task panicked: {join_err}");
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::ClaudeStatusline,
                    result: Err(format!("read task panicked: {join_err}")),
                }))
                .await;
            return;
        }
    };

    match read_result {
        Ok(payload) => {
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::ClaudeStatusline,
                    result: Ok(SourcePartial::ClaudeStatusline(payload)),
                }))
                .await;
        }
        Err(FileIoError::FileMissing { .. }) => {
            // Not an error — user hasn't wired statusLine yet. No emit.
            tracing::debug!("watcher/statusline: snapshot file absent; skipping emit");
        }
        Err(e) => {
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    source: Source::ClaudeStatusline,
                    result: Err(format!("{e}")),
                }))
                .await;
        }
    }
}
