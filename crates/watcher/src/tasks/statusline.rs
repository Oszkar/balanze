//! Statusline notify task. Watches the Balanze data directory for changes to
//! `statusline.snapshot.json` (written by `balanze-cli statusline`), debounces
//! bursts for 100ms, then re-reads and emits the snapshot on each batch.
//!
//! The watch is non-recursive on the data directory - only direct children
//! generate events. On notify init failure the task returns
//! `Err(WatcherError::NotifyExhausted { affected: Source::ClaudeStatusline })`.
//! If the file does not exist (`FileIoError::FileMissing`) no event is emitted;
//! this is the normal state for users who haven't wired the statusLine yet.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use claude_statusline::{FileIoError, read_snapshot};
use notify::{Event, EventKind, RecursiveMode, Watcher as _};
use settings::statusline_snapshot_path;
use state_coordinator::{
    Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg, WatcherGeneration,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::errors::WatcherError;

/// Debounce window for statusline file changes - shorter than the JSONL
/// debounce (300ms) because the statusline file is a single small JSON blob
/// written once per `balanze-cli statusline` invocation, not a stream of
/// many small appends.
const DEBOUNCE: Duration = Duration::from_millis(100);

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
/// `FileMissing` is not emitted - it's the expected state for users who
/// haven't wired `statusLine` in their Claude Code settings yet.
pub(crate) fn spawn(
    coord: StateCoordinatorHandle,
    generation: WatcherGeneration,
) -> JoinHandle<Result<(), WatcherError>> {
    spawn_at(coord, generation, statusline_snapshot_path())
}

/// Start the same watcher against an explicit bridge path, including test fixtures.
fn spawn_at(
    coord: StateCoordinatorHandle,
    generation: WatcherGeneration,
    snapshot_path: Option<PathBuf>,
) -> JoinHandle<Result<(), WatcherError>> {
    tokio::spawn(async move {
        let snapshot_path = match snapshot_path {
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
        // so this startup I/O doesn't block a tokio worker - consistent
        // with the `spawn_blocking` discipline used elsewhere in the
        // watcher for sync FS work. On a slow / remote filesystem this
        // can actually take a measurable moment.
        if let Err(e) = tokio::fs::create_dir_all(&watch_dir).await {
            // `Io(e)` is the right variant - this is a plain filesystem
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
        let watched_path = snapshot_path.clone();
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) if affects_snapshot(&event, &watched_path) => signal_cb.notify_one(),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("watcher/statusline: notify error: {e}");
                    signal_cb.notify_one();
                }
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
        // failure here is a real error (permissions, exhaustion) - treat
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

        // Initial read on task startup - covers the file already existing.
        emit_statusline_snapshot(&coord, &snapshot_path, generation).await;

        loop {
            signal.notified().await;
            tokio::time::sleep(DEBOUNCE).await;
            emit_statusline_snapshot(&coord, &snapshot_path, generation).await;
        }
    })
}

/// Match the bridge name or its current resolved target, ignoring read feedback.
/// Target resolution runs on notify's callback thread, not a Tokio worker, and
/// is repeated so retargeting a symlink does not leave a cached destination stale.
fn affects_snapshot(event: &Event, snapshot_path: &Path) -> bool {
    // Rescan notices carry no reliable paths. Otherwise this non-recursive
    // watch only needs the bridge filename, including either side of a rename.
    // Comparing filenames also tolerates canonicalized parent paths on macOS.
    // Ignore access events: our own read must not trigger another read.
    if event.need_rescan() {
        return true;
    }
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    if event
        .paths
        .iter()
        .any(|path| path.file_name().is_some() && path.file_name() == snapshot_path.file_name())
    {
        return true;
    }

    // atomic_file preserves a bridge symlink and publishes over its resolved
    // target. Compare complete resolved paths so an unrelated file with the
    // same basename cannot turn snapshot publication back into a feedback loop.
    let Ok(target) = snapshot_path.canonicalize() else {
        return false;
    };
    event
        .paths
        .iter()
        .any(|path| path.canonicalize().is_ok_and(|path| path == target))
}

/// Read the statusline snapshot from disk (sync) and emit an update to the
/// coordinator. `FileMissing` is silently swallowed - it means the user hasn't
/// wired statusLine yet and is not an error state.
async fn emit_statusline_snapshot(
    coord: &StateCoordinatorHandle,
    path: &std::path::Path,
    generation: WatcherGeneration,
) {
    let path_owned = path.to_path_buf();
    let result = tokio::task::spawn_blocking(move || read_snapshot(&path_owned)).await;

    let read_result = match result {
        Ok(r) => r,
        Err(join_err) => {
            tracing::error!("watcher/statusline: read task panicked: {join_err}");
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    generation,
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
                    generation,
                    source: Source::ClaudeStatusline,
                    result: Ok(SourcePartial::ClaudeStatusline(payload)),
                }))
                .await;
        }
        Err(FileIoError::FileMissing { .. }) => {
            // Not an error - user hasn't wired statusLine yet. No emit.
            tracing::debug!("watcher/statusline: snapshot file absent; skipping emit");
        }
        Err(e) => {
            let _ = coord
                .send(StateMsg::Update(SourceUpdate {
                    generation,
                    source: Source::ClaudeStatusline,
                    result: Err(format!("{e}")),
                }))
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, Flag, ModifyKind, RenameMode};
    use state_coordinator::{Sink, Snapshot, SnapshotFilePayload, atomic_write_snapshot_file};

    #[test]
    fn filters_reads_and_unrelated_files_but_keeps_rename_and_rescan() {
        let path = PathBuf::from("data/statusline.snapshot.json");
        for event in [
            Event::new(EventKind::Access(AccessKind::Read)).add_path(path.clone()),
            Event::new(EventKind::Create(CreateKind::File))
                .add_path(PathBuf::from("data/snapshot.json")),
            Event::new(EventKind::Create(CreateKind::File))
                .add_path(PathBuf::from("data/statusline.snapshot.json.123.tmp")),
        ] {
            assert!(!affects_snapshot(&event, &path), "{event:?}");
        }
        for event in [
            Event::new(EventKind::Create(CreateKind::File)).add_path(path.clone()),
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                .add_path(PathBuf::from("data/statusline.snapshot.json.123.tmp"))
                .add_path(path.clone()),
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
                .add_path(path.clone()),
            Event::new(EventKind::Other).set_flag(Flag::Rescan),
        ] {
            assert!(affects_snapshot(&event, &path), "{event:?}");
        }
    }

    struct SnapshotSink(tokio::sync::mpsc::UnboundedSender<Snapshot>);

    #[test]
    fn symlink_filter_tracks_retargets_and_rejects_same_named_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("statusline.snapshot.json");
        let first = dir.path().join("bridge-data.json");
        let second = dir.path().join("new-bridge-data.json");
        let other = tempfile::tempdir().unwrap();
        let unrelated = other.path().join("bridge-data.json");
        for target in [&first, &second, &unrelated] {
            std::fs::write(target, b"fixture").unwrap();
        }
        if !create_symlink_or_skip(&first, &path) {
            return;
        }
        let event = |target: &Path| {
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
                .add_path(target.to_path_buf())
        };
        assert!(affects_snapshot(&event(&first), &path));
        assert!(!affects_snapshot(&event(&unrelated), &path));
        assert!(!affects_snapshot(
            &Event::new(EventKind::Access(AccessKind::Read)).add_path(first.clone()),
            &path
        ));
        std::fs::remove_file(&path).unwrap();
        assert!(create_symlink_or_skip(&second, &path));
        assert!(!affects_snapshot(&event(&first), &path));
        assert!(affects_snapshot(&event(&second), &path));
    }

    impl Sink for SnapshotSink {
        fn on_snapshot(&mut self, snapshot: &Snapshot) {
            let _ = self.0.send(snapshot.clone());
        }

        fn on_degraded(&mut self, _: Source, error: &str) {
            panic!("unexpected degradation: {error}");
        }
    }

    #[tokio::test]
    async fn snapshot_publication_does_not_retrigger_bridge_but_replacement_does() {
        exercise_snapshot_publication(false).await;
    }

    #[tokio::test]
    async fn symlink_target_replacement_updates_bridge_without_snapshot_feedback() {
        exercise_snapshot_publication(true).await;
    }

    async fn exercise_snapshot_publication(symlink: bool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("statusline.snapshot.json");
        let mut payload = claude_statusline::StatuslineFilePayload::new(
            claude_statusline::parse(r#"{"cost":{"total_cost_usd":1}}"#).unwrap(),
            chrono::Utc::now(),
        );
        if symlink {
            let target = dir.path().join("bridge-data.json");
            claude_statusline::atomic_write_snapshot(&target, &payload).unwrap();
            if !create_symlink_or_skip(Path::new("bridge-data.json"), &path) {
                return;
            }
            let event =
                Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To))).add_path(target);
            assert!(affects_snapshot(&event, &path));
        } else {
            claude_statusline::atomic_write_snapshot(&path, &payload).unwrap();
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (coord, coord_join) = state_coordinator::spawn(SnapshotSink(tx));
        let watcher = spawn_at(coord.clone(), 0, Some(path.clone()));
        let initial = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(initial.claude_statusline.as_ref(), Some(&payload));

        // FSEvents can deliver the buffered seed write after the explicit
        // initial read. Establish a quiet stream before measuring whether
        // snapshot publication causes a new bridge update.
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Ok(snapshot) =
                tokio::time::timeout(Duration::from_millis(700), rx.recv()).await
            {
                assert_eq!(snapshot.unwrap().claude_statusline.as_ref(), Some(&payload));
            }
        })
        .await
        .expect("statusline watcher did not settle after startup");

        // Reproduce the other half of the bridge with its real atomic writer.
        // These temp/create/rename events previously re-ingested the unchanged
        // statusline and would trigger another publication indefinitely.
        atomic_write_snapshot_file(
            &dir.path().join("snapshot.json"),
            &SnapshotFilePayload::new(initial, chrono::Utc::now()),
        )
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(700), rx.recv())
                .await
                .is_err()
        );

        payload.payload.session_cost_micro_usd = Some(2_000_000);
        payload.captured_at = chrono::Utc::now();
        claude_statusline::atomic_write_snapshot(&path, &payload).unwrap();
        let updated = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.claude_statusline.as_ref(), Some(&payload));
        if symlink {
            assert!(std::fs::symlink_metadata(&path).unwrap().is_symlink());
        }

        watcher.abort();
        assert!(watcher.await.unwrap_err().is_cancelled());
        drop(coord);
        coord_join.await.unwrap();
    }

    #[cfg(unix)]
    fn create_symlink_or_skip(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).unwrap();
        true
    }

    #[cfg(windows)]
    fn create_symlink_or_skip(target: &Path, link: &Path) -> bool {
        match std::os::windows::fs::symlink_file(target, link) {
            Ok(()) => true,
            Err(error) if error.raw_os_error() == Some(1314) => {
                eprintln!("skipping symlink test: Windows symlink privilege is unavailable");
                false
            }
            Err(error) => panic!("failed to create symlink fixture: {error}"),
        }
    }
}
