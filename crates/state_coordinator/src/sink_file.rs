//! `SnapshotFileSink` - the WRITE side of the cross-provider Hybrid read path.
//!
//! Wraps any `Sink`; on each `on_snapshot` it persists the full `Snapshot` to
//! `snapshot.json` (best-effort, errors logged not propagated) then delegates to
//! the inner sink. The host (Tauri app / `balanze-cli watch`) wraps its real
//! sink with this; the one-shot `balanze-cli statusline` reads the file. Keeps
//! file I/O on the sink side, never in the coordinator actor (boundary #7).

use std::path::PathBuf;

use chrono::Utc;

use crate::messages::Source;
use crate::sink::Sink;
use crate::snapshot::Snapshot;
use crate::snapshot_file::{SnapshotFilePayload, atomic_write_snapshot_file};

pub struct SnapshotFileSink<S: Sink> {
    inner: S,
    path: PathBuf,
}

impl<S: Sink> SnapshotFileSink<S> {
    pub fn new(inner: S, path: PathBuf) -> Self {
        Self { inner, path }
    }
}

impl<S: Sink> Sink for SnapshotFileSink<S> {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        let payload = SnapshotFilePayload::new(snapshot.clone(), Utc::now());
        if let Err(e) = atomic_write_snapshot_file(&self.path, &payload) {
            tracing::warn!("snapshot.json write failed: {e}");
        }
        self.inner.on_snapshot(snapshot);
    }

    fn on_degraded(&mut self, source: Source, error: &str) {
        self.inner.on_degraded(source, error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Snapshot;
    use crate::snapshot_file::read_snapshot_file;
    use chrono::TimeZone as _;
    use tempfile::tempdir;

    #[derive(Default)]
    struct CountingSink {
        snapshots: usize,
        degraded: usize,
    }
    impl Sink for CountingSink {
        fn on_snapshot(&mut self, _s: &Snapshot) {
            self.snapshots += 1;
        }
        fn on_degraded(&mut self, _src: Source, _e: &str) {
            self.degraded += 1;
        }
    }

    fn snap() -> Snapshot {
        Snapshot::empty(chrono::Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).unwrap())
    }

    #[test]
    fn writes_file_and_delegates_on_snapshot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let mut sink = SnapshotFileSink::new(CountingSink::default(), path.clone());
        sink.on_snapshot(&snap());
        assert_eq!(sink.inner.snapshots, 1, "inner sink still called");
        let back = read_snapshot_file(&path).expect("file written");
        assert_eq!(
            back.schema_version,
            crate::snapshot::SNAPSHOT_SCHEMA_VERSION
        );
    }

    #[test]
    fn on_degraded_delegates() {
        let dir = tempdir().unwrap();
        let mut sink =
            SnapshotFileSink::new(CountingSink::default(), dir.path().join("snapshot.json"));
        sink.on_degraded(Source::CodexQuota, "boom");
        assert_eq!(sink.inner.degraded, 1);
    }

    #[test]
    fn write_failure_does_not_break_inner() {
        // Parent is a FILE, so create_dir_all / create fails -> write errors,
        // but the inner sink must still be called and nothing panics.
        let dir = tempdir().unwrap();
        let file_as_parent = dir.path().join("a_file");
        std::fs::write(&file_as_parent, b"x").unwrap();
        let bad_path = file_as_parent.join("snapshot.json");
        let mut sink = SnapshotFileSink::new(CountingSink::default(), bad_path);
        sink.on_snapshot(&snap()); // must not panic
        assert_eq!(
            sink.inner.snapshots, 1,
            "inner sink still called on write failure"
        );
    }
}
