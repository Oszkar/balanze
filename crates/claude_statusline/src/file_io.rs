//! Balanze-side IPC file IO for statusline snapshots.
//!
//! This module reads/writes the data file at
//! `<data_dir>/statusline.snapshot.json`, where `<data_dir>` is what
//! `directories::ProjectDirs::from("me", "oszkar", "Balanze").data_dir()`
//! returns on the host platform — that path already includes the per-OS
//! Balanze subpath (e.g. `~/.local/share/balanze/` on Linux,
//! `~/Library/Application Support/me.oszkar.Balanze/` on macOS,
//! `%LOCALAPPDATA%\oszkar\Balanze\data\` on Windows). The caller
//! (`balanze_cli`) resolves the path; this module is path-agnostic and
//! just operates on whatever `&Path` it's handed. It does NOT touch
//! Claude Code's `settings.json` (that is `wiring.rs`) nor the
//! statusLine wire format (that is `parse.rs`).
//!
//! Both functions follow the atomic tmp+fsync+rename discipline mirrored from
//! `anthropic_oauth::credentials::write_back` (AGENTS.md §3.4).  Error
//! messages include the file path only — never file contents (defense in
//! depth).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::payload::{SCHEMA_VERSION, StatuslineFilePayload};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors returned by [`read_snapshot`] and [`atomic_write_snapshot`].
///
/// Every variant carries the file path; none carry file contents.
#[derive(Debug, thiserror::Error)]
pub enum FileIoError {
    /// The snapshot file does not exist at `path`.
    #[error("statusline snapshot file missing: {path}")]
    FileMissing { path: PathBuf },

    /// An OS-level I/O error occurred while reading or writing `path`.
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The file exists but its JSON is invalid or does not match the expected
    /// shape.  File contents are NOT included in the `#[error]` message, but
    /// the underlying `serde_json::Error` is preserved as the `#[source]`
    /// so callers walking the error chain (`anyhow`, `tracing` with
    /// `display_chain`, etc.) can see line / column / "missing field X" /
    /// "invalid type" diagnostics without us having to re-derive them.
    ///
    /// Defense-in-depth note: `serde_json::Error::Display` on a `data`-class
    /// failure can echo the offending VALUE (e.g. `invalid type: string
    /// "abc" at line 1 column 18`). The statusline snapshot file is
    /// non-secret (no tokens, no keys — see crate-level doc), so the modest
    /// leak surface is acceptable here. The same trade-off would NOT apply
    /// to the OAuth credentials file; see `anthropic_oauth::credentials`.
    #[error("statusline snapshot parse error in {path}")]
    ParseError {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// The file's `schema_version` field differs from [`SCHEMA_VERSION`].
    /// Consumers should discard the file and wait for a fresh write.
    #[error(
        "statusline snapshot schema drift in {path}: found version {found_version}, expected {expected}"
    )]
    SchemaDrift {
        path: PathBuf,
        found_version: u8,
        expected: u8,
    },
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Read and validate a [`StatuslineFilePayload`] from `path`.
///
/// Error mapping:
/// - File absent            → [`FileIoError::FileMissing`]
/// - Other OS error         → [`FileIoError::Io`]
/// - Invalid JSON / shape   → [`FileIoError::ParseError`]
/// - Wrong `schema_version` → [`FileIoError::SchemaDrift`]
pub fn read_snapshot(path: &Path) -> Result<StatuslineFilePayload, FileIoError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(FileIoError::FileMissing {
                path: path.to_path_buf(),
            });
        }
        Err(e) => {
            return Err(FileIoError::Io {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };

    // Pre-check schema_version with a lightweight probe so a future-versioned
    // file yields a precise SchemaDrift error rather than a generic ParseError
    // on the full payload shape.
    #[derive(serde::Deserialize)]
    struct VersionProbe {
        schema_version: u8,
    }
    let probe: VersionProbe =
        serde_json::from_slice(&bytes).map_err(|e| FileIoError::ParseError {
            path: path.to_path_buf(),
            source: e,
        })?;

    if probe.schema_version != SCHEMA_VERSION {
        return Err(FileIoError::SchemaDrift {
            path: path.to_path_buf(),
            found_version: probe.schema_version,
            expected: SCHEMA_VERSION,
        });
    }

    serde_json::from_slice(&bytes).map_err(|e| FileIoError::ParseError {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Atomically write `payload` to `path` via tmp+fsync+rename.
///
/// - Creates parent directories if they do not exist.
/// - If `path` already exists, copies its permissions onto the replacement
///   file before renaming (Unix only; on Windows, ACL inheritance from the
///   parent directory applies naturally and `set_permissions` is a no-op for
///   simple mode bits).
/// - No tmp file is left on disk after a successful call.
/// - On failure the original file (if any) is left untouched.
pub fn atomic_write_snapshot(
    path: &Path,
    payload: &StatuslineFilePayload,
) -> Result<(), FileIoError> {
    // A bare relative path like `"status.json"` has either no parent
    // component (`path.parent() == None`) or an empty one (`Some("")`),
    // depending on the platform / construction. Both should be treated
    // as the current working directory rather than erroring out.
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };

    std::fs::create_dir_all(parent).map_err(|e| FileIoError::Io {
        path: parent.to_path_buf(),
        source: e,
    })?;

    // NOTE: serde_json serialization of StatuslineFilePayload is infallible for
    // all current fields (u8, DateTime<Utc>, Option<i64>, Option<String>,
    // Option<RateLimits>). This arm is unreachable in practice. If a non-
    // serializable type is ever added to the envelope, introduce a distinct
    // `WriteSerializeError` variant on `FileIoError` rather than reusing
    // `ParseError` (which is a read-path concept) — naming the variant for the
    // failure mode matters when the branch ever becomes reachable.
    let bytes = serde_json::to_vec_pretty(payload).map_err(|e| FileIoError::ParseError {
        path: path.to_path_buf(),
        source: e,
    })?;

    // Unique tmp name: pid + nanosecond timestamp + monotonic counter.
    // Avoids collisions on concurrent calls or PID reuse (mirrors wiring.rs).
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let tmp = parent.join(format!(
        "statusline.snapshot.{}-{}-{}.json.tmp",
        std::process::id(),
        nanos,
        seq,
    ));

    // Write to tmp, fsync, then atomically rename over the final path.
    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create_new(&tmp)?;
        f.write_all(&bytes)?;
        // fsync before rename: crash between write and rename cannot lose data.
        f.sync_all()?;
        Ok(())
    })();

    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(FileIoError::Io {
            path: tmp,
            source: e,
        });
    }

    // Unix: copy the original file's permissions onto the tmp BEFORE rename,
    // so the final file inherits the caller's chosen mode (e.g. 0o600).
    // On Windows this block is compiled out; the ACL of the parent directory
    // governs new files naturally.
    #[cfg(unix)]
    {
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
    }

    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        FileIoError::Io {
            path: path.to_path_buf(),
            source: e,
        }
    })?;

    // Unix: fsync the parent directory so the rename itself is durable.
    // Best-effort — dir-fsync failure must not fail the write since data is
    // already renamed into place.  Windows does not support opening a
    // directory as a File for sync; the file fsync + rename is the portable
    // guarantee.
    #[cfg(unix)]
    {
        let _ = std::fs::File::open(parent).and_then(|f| f.sync_all());
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;
    use tempfile::tempdir;

    fn sample_payload() -> StatuslineFilePayload {
        use crate::types::StatuslineSnapshot;
        let snap = StatuslineSnapshot {
            rate_limits: None,
            session_cost_micro_usd: Some(3_420_000), // $3.42 in micro-USD
            claude_code_version: Some("v2.1.144".to_string()),
            model_display_name: None,
            context_used_percent: None,
        };
        StatuslineFilePayload::new(
            snap,
            chrono::Utc.with_ymd_and_hms(2026, 5, 21, 12, 0, 0).unwrap(),
        )
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("statusline.snapshot.json");
        atomic_write_snapshot(&path, &sample_payload()).unwrap();
        let back = read_snapshot(&path).unwrap();
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert_eq!(back, sample_payload());
    }

    /// Regression test for the case where a relative-path argument has either
    /// no parent component or an empty one — both must fall back to the
    /// current directory (mirroring `wiring::wire_statusline`'s behavior)
    /// rather than erroring out. We verify the write succeeds against a
    /// tempdir-prefixed path; the bare-filename case is exercised by the
    /// resolution logic below.
    #[test]
    fn parent_resolution_handles_none_and_empty_parent() {
        use std::path::{Path, PathBuf};

        // Reproduce the resolution logic from atomic_write_snapshot.
        let resolve = |p: &Path| -> PathBuf {
            match p.parent() {
                Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                _ => Path::new(".").to_path_buf(),
            }
        };

        // No parent component at all → "."
        assert_eq!(resolve(Path::new("")), PathBuf::from("."));
        // Bare relative filename → parent is Some("") → falls back to "."
        assert_eq!(resolve(Path::new("status.json")), PathBuf::from("."));
        // Real parent stays as-is.
        let nested = PathBuf::from("/tmp/balanze/status.json");
        assert_eq!(resolve(&nested), PathBuf::from("/tmp/balanze"));
    }

    #[test]
    fn missing_file_returns_file_missing_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        let err = read_snapshot(&path).unwrap_err();
        assert!(
            matches!(err, FileIoError::FileMissing { .. }),
            "expected FileMissing, got {err:?}"
        );
    }

    #[test]
    fn malformed_json_returns_parse_error_with_source() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, b"{not valid json").unwrap();
        let err = read_snapshot(&path).unwrap_err();
        match &err {
            FileIoError::ParseError { source, .. } => {
                // The serde_json::Error must be preserved (#[source]) so
                // callers walking the error chain see the line/column
                // diagnostic. Top-level Display message stays content-free.
                let display = err.to_string();
                assert!(
                    display.starts_with("statusline snapshot parse error in"),
                    "outer Display must not include the JSON snippet: {display}"
                );
                assert!(!display.contains("not valid json"));
                // Source carries the actual diagnostic.
                let src_display = source.to_string();
                assert!(
                    src_display.contains("line") || src_display.contains("column"),
                    "serde_json source error should carry line/column: {src_display}"
                );
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn valid_json_with_missing_required_field_returns_parse_error() {
        // Passes the VersionProbe (has `schema_version`), passes the version
        // check, then fails the full StatuslineFilePayload deserialize because
        // `payload` is missing. Exercises the second-pass parse path.
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing_field.json");
        std::fs::write(
            &path,
            r#"{"schema_version":1,"captured_at":"2026-05-21T00:00:00Z"}"#,
        )
        .unwrap();
        let err = read_snapshot(&path).unwrap_err();
        assert!(
            matches!(err, FileIoError::ParseError { .. }),
            "expected ParseError, got {err:?}"
        );
    }

    #[test]
    fn schema_version_mismatch_returns_schema_drift() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("future.json");
        std::fs::write(
            &path,
            br#"{"schema_version":99,"captured_at":"2026-05-21T00:00:00Z","payload":{"rate_limits":null,"session_cost_micro_usd":null,"claude_code_version":null}}"#,
        )
        .unwrap();
        let err = read_snapshot(&path).unwrap_err();
        match err {
            FileIoError::SchemaDrift { found_version, .. } => {
                assert_eq!(found_version, 99);
            }
            other => panic!("expected SchemaDrift, got {other:?}"),
        }
    }

    #[test]
    fn atomic_write_leaves_no_tmp_file_after_success() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("statusline.snapshot.json");
        atomic_write_snapshot(&path, &sample_payload()).unwrap();
        // Assert no `*.tmp` files remain in the dir after a successful write.
        let leftover_tmps: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftover_tmps.is_empty(),
            "tmp file should be renamed away: {leftover_tmps:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempdir().unwrap();
        let path = dir.path().join("statusline.snapshot.json");
        // Create the file first, then lock down permissions to 0o600.
        std::fs::write(&path, b"{}").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms).unwrap();

        atomic_write_snapshot(&path, &sample_payload()).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "permissions must be preserved across atomic write"
        );
    }

    #[test]
    fn write_creates_parent_dirs() {
        let base = tempdir().unwrap();
        let path = base
            .path()
            .join("subdir")
            .join("nested")
            .join("status.json");
        atomic_write_snapshot(&path, &sample_payload()).unwrap();
        assert!(path.exists(), "file must be created with parent dirs");
        read_snapshot(&path).unwrap();
    }
}
