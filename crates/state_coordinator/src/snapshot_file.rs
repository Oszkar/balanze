//! IPC file for the cross-provider `Snapshot`: `<data_dir>/snapshot.json`.
//!
//! The OPPOSITE direction to `claude_statusline`'s `statusline.snapshot.json`:
//! the host that owns the coordinator (the Tauri app, or `balanze-cli watch`)
//! WRITES the live `Snapshot` on every coordinator update via `SnapshotFileSink`
//! (added in the next task), and the one-shot `balanze-cli statusline` process
//! READS it to fill the cross-provider (Codex / OpenAI) segments without any
//! network I/O of its own (AGENTS.md §3.1; the statusline design's Hybrid read
//! path).
//!
//! Atomic tmp+fsync+rename write, probe-then-parse read, path-only errors -
//! mirrors `claude_statusline::file_io`. The coordinator actor never calls these
//! (boundary #7: no I/O in the actor); the host's sink does.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::snapshot::{SNAPSHOT_SCHEMA_VERSION, Snapshot};

/// Versioned envelope written to `snapshot.json`. `captured_at` is the
/// consumer-side freshness signal (the statusline reader compares its age to a
/// TTL). `snapshot` is the full coordinator snapshot, serde round-trippable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFilePayload {
    pub schema_version: u32,
    pub captured_at: DateTime<Utc>,
    pub snapshot: Snapshot,
}

impl SnapshotFilePayload {
    pub fn new(snapshot: Snapshot, captured_at: DateTime<Utc>) -> Self {
        Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            captured_at,
            snapshot,
        }
    }
}

/// Errors from [`read_snapshot_file`] / [`atomic_write_snapshot_file`]. Every
/// variant carries the path; none carry file contents.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotFileError {
    #[error("snapshot file missing: {path}")]
    FileMissing { path: PathBuf },
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("snapshot parse error in {path}")]
    ParseError {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("snapshot schema drift in {path}: found version {found_version}, expected {expected}")]
    SchemaDrift {
        path: PathBuf,
        found_version: u32,
        expected: u32,
    },
}

/// Resolve `<data_dir>/snapshot.json`. Honors `BALANZE_DATA_DIR_OVERRIDE`
/// (tests / headless) exactly like the statusline snapshot path. `None` when no
/// project dir resolves.
pub fn snapshot_file_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("BALANZE_DATA_DIR_OVERRIDE") {
        return Some(PathBuf::from(dir).join("snapshot.json"));
    }
    directories::ProjectDirs::from("me", "oszkar", "Balanze")
        .map(|d| d.data_dir().join("snapshot.json"))
}

/// Read + validate a [`SnapshotFilePayload`]. Probe `schema_version` first so a
/// future-versioned file yields a precise `SchemaDrift` rather than a generic
/// parse error.
pub fn read_snapshot_file(path: &Path) -> Result<SnapshotFilePayload, SnapshotFileError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(SnapshotFileError::FileMissing {
                path: path.to_path_buf(),
            });
        }
        Err(e) => {
            return Err(SnapshotFileError::Io {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };

    #[derive(Deserialize)]
    struct VersionProbe {
        schema_version: u32,
    }
    let probe: VersionProbe =
        serde_json::from_slice(&bytes).map_err(|e| SnapshotFileError::ParseError {
            path: path.to_path_buf(),
            source: e,
        })?;
    if probe.schema_version != SNAPSHOT_SCHEMA_VERSION {
        return Err(SnapshotFileError::SchemaDrift {
            path: path.to_path_buf(),
            found_version: probe.schema_version,
            expected: SNAPSHOT_SCHEMA_VERSION,
        });
    }

    serde_json::from_slice(&bytes).map_err(|e| SnapshotFileError::ParseError {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Atomically write `payload` via tmp+fsync+rename. Creates parent dirs; leaves
/// no tmp on success; preserves existing perms on unix.
pub fn atomic_write_snapshot_file(
    path: &Path,
    payload: &SnapshotFilePayload,
) -> Result<(), SnapshotFileError> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent).map_err(|e| SnapshotFileError::Io {
        path: parent.to_path_buf(),
        source: e,
    })?;

    // NOTE: serde_json serialization of SnapshotFilePayload is infallible for
    // all current envelope fields (u32, DateTime<Utc>, and the Snapshot cells,
    // which are plain serde structs / Options). This arm is unreachable in
    // practice. `ParseError` is a read-path concept reused here for a write-side
    // serialization failure; if a non-serializable field is ever added to the
    // envelope, introduce a distinct `WriteSerializeError` variant rather than
    // reusing `ParseError` - naming the variant for the failure mode matters
    // once the branch becomes reachable.
    let bytes = serde_json::to_vec_pretty(payload).map_err(|e| SnapshotFileError::ParseError {
        path: path.to_path_buf(),
        source: e,
    })?;

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let tmp = parent.join(format!(
        "snapshot.{}-{}-{}.json.tmp",
        std::process::id(),
        nanos,
        seq,
    ));

    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create_new(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(SnapshotFileError::Io {
            path: tmp,
            source: e,
        });
    }

    #[cfg(unix)]
    {
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
    }

    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        SnapshotFileError::Io {
            path: path.to_path_buf(),
            source: e,
        }
    })?;

    #[cfg(unix)]
    {
        let _ = std::fs::File::open(parent).and_then(|f| f.sync_all());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Snapshot;
    use chrono::TimeZone as _;
    use tempfile::tempdir;

    fn payload_with_codex() -> SnapshotFilePayload {
        let now = chrono::Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).unwrap();
        let mut snap = Snapshot::empty(now);
        snap.codex_quota = Some(codex_local::types::CodexQuotaSnapshot {
            observed_at: now,
            session_id: "s".into(),
            primary: codex_local::types::RateLimitWindow {
                used_percent: 6.0,
                window_duration_minutes: 10_080,
                resets_at: now,
            },
            secondary: None,
            plan_type: "go".into(),
            rate_limit_reached: false,
        });
        SnapshotFilePayload::new(snap, now)
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        atomic_write_snapshot_file(&path, &payload_with_codex()).unwrap();
        let back = read_snapshot_file(&path).unwrap();
        assert_eq!(back.schema_version, SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(back.snapshot.codex_quota.unwrap().primary.used_percent, 6.0);
    }

    #[test]
    fn missing_file_is_file_missing() {
        let dir = tempdir().unwrap();
        let err = read_snapshot_file(&dir.path().join("nope.json")).unwrap_err();
        assert!(
            matches!(err, SnapshotFileError::FileMissing { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn wrong_schema_version_is_drift() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        std::fs::write(
            &path,
            br#"{"schema_version":999,"captured_at":"2026-06-30T12:00:00Z","snapshot":{}}"#,
        )
        .unwrap();
        match read_snapshot_file(&path).unwrap_err() {
            SnapshotFileError::SchemaDrift { found_version, .. } => assert_eq!(found_version, 999),
            other => panic!("expected SchemaDrift, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_is_parse_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(
            read_snapshot_file(&path).unwrap_err(),
            SnapshotFileError::ParseError { .. }
        ));
    }

    #[test]
    fn no_tmp_left_after_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        atomic_write_snapshot_file(&path, &payload_with_codex()).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn path_honors_env_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("BALANZE_DATA_DIR_OVERRIDE").ok();
        // SAFETY: ENV_LOCK serializes every env-mutating test in this module;
        // set_var/remove_var are unsafe as of edition 2024.
        unsafe { std::env::set_var("BALANZE_DATA_DIR_OVERRIDE", "/tmp/balanze-x") };
        let p = snapshot_file_path().unwrap();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("BALANZE_DATA_DIR_OVERRIDE", v),
                None => std::env::remove_var("BALANZE_DATA_DIR_OVERRIDE"),
            }
        }
        assert!(p.ends_with("snapshot.json"));
        assert!(p.to_string_lossy().contains("balanze-x"));
    }

    #[test]
    fn parent_resolution_handles_none_and_empty_parent() {
        use std::path::{Path, PathBuf};
        let resolve = |p: &Path| -> PathBuf {
            match p.parent() {
                Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                _ => Path::new(".").to_path_buf(),
            }
        };
        assert_eq!(resolve(Path::new("")), PathBuf::from("."));
        assert_eq!(resolve(Path::new("snapshot.json")), PathBuf::from("."));
        assert_eq!(
            resolve(&PathBuf::from("/tmp/balanze/snapshot.json")),
            PathBuf::from("/tmp/balanze")
        );
    }

    #[test]
    fn valid_json_with_missing_required_field_is_parse_error() {
        // Passes the version probe (schema_version tracks SNAPSHOT_SCHEMA_VERSION,
        // not a hardcoded literal - a stale literal here would silently start
        // exercising the SchemaDrift branch instead on the next version bump),
        // then fails the full SnapshotFilePayload parse because `snapshot` is
        // missing - exercises the second from_slice path.
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"schema_version":{SNAPSHOT_SCHEMA_VERSION},"captured_at":"2026-06-30T12:00:00Z"}}"#
            ),
        )
        .unwrap();
        assert!(matches!(
            read_snapshot_file(&path).unwrap_err(),
            SnapshotFileError::ParseError { .. }
        ));
    }

    #[test]
    fn write_creates_parent_dirs() {
        let base = tempdir().unwrap();
        let path = base.path().join("sub").join("nested").join("snapshot.json");
        atomic_write_snapshot_file(&path, &payload_with_codex()).unwrap();
        assert!(path.exists());
        read_snapshot_file(&path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        std::fs::write(&path, b"{}").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms).unwrap();
        atomic_write_snapshot_file(&path, &payload_with_codex()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
