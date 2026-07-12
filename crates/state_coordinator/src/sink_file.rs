//! Nonblocking `snapshot.json` publication.
//!
//! The coordinator publishes immutable payloads through a `tokio::sync::watch`
//! channel. A dedicated task performs serialization and durable file I/O on a
//! blocking worker. `watch` retains only the newest pending payload while a
//! write is in progress, so slow storage cannot stall the actor or create an
//! unbounded queue.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::snapshot::Snapshot;
use crate::snapshot_file::{SnapshotFilePayload, atomic_write_snapshot_file};

#[derive(Clone)]
struct PendingSnapshot {
    sequence: u64,
    payload: SnapshotFilePayload,
}

pub(crate) type WriteFn =
    Arc<dyn Fn(&Path, &SnapshotFilePayload) -> Result<(), String> + Send + Sync>;

/// Actor-side publisher. `publish` only clones the snapshot and replaces the
/// single pending watch value; it never serializes or touches the filesystem.
pub(crate) struct SnapshotPublisher {
    tx: watch::Sender<Option<PendingSnapshot>>,
    next_sequence: u64,
}

impl SnapshotPublisher {
    pub(crate) fn publish(&mut self, snapshot: &Snapshot) {
        let pending = PendingSnapshot {
            sequence: self.next_sequence,
            // Preserve the source merge timestamp. Refresh and settings
            // re-notifications must not extend the file freshness window.
            payload: SnapshotFilePayload::new(snapshot.clone(), snapshot.fetched_at),
        };
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.tx.send_replace(Some(pending));
    }
}

pub(crate) struct SnapshotWriter {
    join: JoinHandle<()>,
}

impl SnapshotWriter {
    pub(crate) async fn shutdown(self) {
        if let Err(error) = self.join.await {
            tracing::error!("snapshot writer task failed during shutdown: {error}");
        }
    }
}

pub(crate) fn spawn_snapshot_writer(path: PathBuf) -> (SnapshotPublisher, SnapshotWriter) {
    spawn_snapshot_writer_with(
        path,
        Arc::new(|path, payload| {
            atomic_write_snapshot_file(path, payload).map_err(|error| error.to_string())
        }),
    )
}

pub(crate) fn spawn_snapshot_writer_with(
    path: PathBuf,
    write: WriteFn,
) -> (SnapshotPublisher, SnapshotWriter) {
    let (tx, rx) = watch::channel(None);
    let join = tokio::spawn(run_writer(path, rx, write));
    (
        SnapshotPublisher {
            tx,
            next_sequence: 1,
        },
        SnapshotWriter { join },
    )
}

async fn run_writer(
    path: PathBuf,
    mut rx: watch::Receiver<Option<PendingSnapshot>>,
    write: WriteFn,
) {
    let mut written_sequence = 0;
    loop {
        let changed = rx.changed().await;
        let pending = rx.borrow_and_update().clone();

        if let Some(pending) = pending.filter(|value| value.sequence > written_sequence) {
            let write_path = path.clone();
            let write_fn = Arc::clone(&write);
            let payload = pending.payload;
            let result = tokio::task::spawn_blocking(move || write_fn(&write_path, &payload)).await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!("snapshot.json write failed: {error}"),
                Err(error) => tracing::error!("snapshot.json blocking writer failed: {error}"),
            }
            written_sequence = pending.sequence;
        }

        if changed.is_err() {
            // All publishers are gone. The borrow above observes the final
            // watch value, including one published while the previous blocking
            // write was in flight, so returning here explicitly flushes or
            // reports the final pending state.
            return;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::snapshot_file::read_snapshot_file;
    use chrono::{TimeZone as _, Utc};
    use std::sync::{Condvar, Mutex};
    use tempfile::tempdir;

    fn snapshot(minute: u32) -> Snapshot {
        Snapshot::empty(Utc.with_ymd_and_hms(2026, 6, 30, 12, minute, 0).unwrap())
    }

    #[tokio::test]
    async fn shutdown_flushes_final_snapshot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let (mut publisher, writer) = spawn_snapshot_writer(path.clone());
        publisher.publish(&snapshot(7));
        drop(publisher);
        writer.shutdown().await;

        let persisted = read_snapshot_file(&path).unwrap();
        assert_eq!(persisted.captured_at, snapshot(7).fetched_at);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocked_write_coalesces_to_latest_snapshot() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let writes = Arc::new(Mutex::new(Vec::new()));
        let entered = Arc::new((Mutex::new(false), Condvar::new()));

        let write = {
            let gate = Arc::clone(&gate);
            let writes = Arc::clone(&writes);
            let entered = Arc::clone(&entered);
            Arc::new(move |_path: &Path, payload: &SnapshotFilePayload| {
                {
                    let (lock, ready) = &*entered;
                    *lock.lock().unwrap() = true;
                    ready.notify_all();
                }
                let (lock, ready) = &*gate;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = ready.wait(open).unwrap();
                }
                writes.lock().unwrap().push(payload.captured_at);
                Ok(())
            }) as WriteFn
        };

        let (mut publisher, writer) = spawn_snapshot_writer_with(PathBuf::from("unused"), write);
        publisher.publish(&snapshot(1));
        {
            let (lock, ready) = &*entered;
            let mut is_entered = lock.lock().unwrap();
            while !*is_entered {
                is_entered = ready.wait(is_entered).unwrap();
            }
        }
        publisher.publish(&snapshot(2));
        publisher.publish(&snapshot(3));
        {
            let (lock, ready) = &*gate;
            *lock.lock().unwrap() = true;
            ready.notify_all();
        }
        drop(publisher);
        writer.shutdown().await;

        let writes = writes.lock().unwrap();
        assert_eq!(
            writes.as_slice(),
            &[snapshot(1).fetched_at, snapshot(3).fetched_at]
        );
    }
}
