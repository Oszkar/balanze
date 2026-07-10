//! Perms-preserving, durable atomic file replace.
//!
//! One home for the subtle sequence AGENTS.md 3.4 relies on, previously
//! copy-pasted across the snapshot, statusline, settings, and OAuth-credential
//! writers where a copy risked silently dropping the directory fsync or a
//! perms-preserving step:
//!
//! 1. create a unique `*.tmp` sibling in the target's directory with
//!    `O_CREAT | O_EXCL` (so two writers never share a tmp),
//! 2. write the bytes and `sync_all` the tmp (a crash between write and rename
//!    cannot lose data),
//! 3. on unix, copy the existing target's permissions onto the tmp (preserve
//!    the file's mode across the replace),
//! 4. `rename` the tmp over the target (atomic on the same filesystem),
//! 5. on unix, fsync the parent directory so the rename itself is durable.
//!
//! The tmp is removed on any failure. Windows has no portable directory fsync
//! and no unix mode; there the file fsync + rename is the durability guarantee
//! and new files inherit the parent directory's ACL.
//!
//! This crate owns ONLY the byte-level write. Callers that must merge into an
//! existing file (e.g. touch only certain JSON fields, or never regress a
//! concurrently-newer on-disk value) do that read/merge/serialize themselves
//! and hand the final bytes here.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Permission policy for the freshly created temp file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permissions {
    /// The OS default for a new file. On unix the existing target's mode is
    /// still copied onto the tmp before the rename (so an existing file's
    /// permissions are preserved); a brand-new file gets the umask default.
    Default,
    /// Create the tmp `0o600` (owner read/write only) on unix, for secret
    /// files. A no-op on non-unix, where the parent directory ACL governs.
    OwnerOnly,
}

/// Atomically replace `path`'s contents with `bytes`. See the crate docs for
/// the exact sequence. The parent directory must already exist.
///
/// Returns the underlying [`io::Error`] on failure (callers map it to their own
/// error type); the tmp file is cleaned up first.
pub fn atomic_write(path: &Path, bytes: &[u8], perms: Permissions) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic_write: path has no parent directory",
        )
    })?;
    let tmp = parent.join(tmp_name(path));

    let write_result = (|| -> io::Result<()> {
        let mut f = create_tmp(&tmp, perms)?;
        f.write_all(bytes)?;
        // fsync before rename: a crash/power-loss between write and rename
        // cannot lose the bytes.
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    // Preserve the existing target's permissions on unix. The tmp already
    // carries a safe mode (umask default, or 0o600 for OwnerOnly), so a copy
    // failure is non-fatal - we keep the restrictive default rather than fail.
    #[cfg(unix)]
    {
        if let Ok(meta) = fs::metadata(path) {
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
    }

    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    // fsync the parent directory so the rename is durable (unix only; Windows
    // cannot open a directory as a File for sync). Best-effort: the data is
    // already renamed into place, so a dir-fsync failure must not fail the write.
    #[cfg(unix)]
    {
        let _ = fs::File::open(parent).and_then(|f| f.sync_all());
    }
    Ok(())
}

#[cfg(unix)]
fn create_tmp(tmp: &Path, perms: Permissions) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    if perms == Permissions::OwnerOnly {
        opts.mode(0o600);
    }
    opts.open(tmp)
}

#[cfg(not(unix))]
fn create_tmp(tmp: &Path, _perms: Permissions) -> io::Result<fs::File> {
    fs::File::create_new(tmp)
}

/// A unique tmp filename in the target's directory:
/// `<target-name>.<pid>-<nanos>-<seq>.tmp`. The pid + monotonic seq + clock
/// nanos make concurrent writers (same or different processes) pick distinct
/// tmps, so the `create_new` above never collides.
fn tmp_name(path: &Path) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let base = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("atomic");
    format!("{base}.{}-{}-{}.tmp", std::process::id(), nanos, seq)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn writes_a_new_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("new.json");
        atomic_write(&p, b"hello", Permissions::Default).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"hello");
    }

    #[test]
    fn overwrites_existing_file_with_new_contents() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("f.json");
        fs::write(&p, b"old-and-longer").unwrap();
        atomic_write(&p, b"new", Permissions::Default).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
    }

    #[test]
    fn leaves_no_tmp_behind_on_success() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("f.json");
        atomic_write(&p, b"x", Permissions::Default).unwrap();
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "tmp files left: {leftovers:?}");
    }

    #[test]
    fn missing_parent_directory_is_an_error() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("nope").join("f.json");
        assert!(atomic_write(&p, b"x", Permissions::Default).is_err());
    }

    #[test]
    fn concurrent_unique_tmp_names() {
        // Two names generated back to back must differ (seq counter), so
        // create_new can't collide between concurrent writers.
        let p = Path::new("/some/dir/target.json");
        assert_ne!(tmp_name(p), tmp_name(p));
    }

    #[cfg(unix)]
    #[test]
    fn owner_only_creates_0o600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempdir().unwrap();
        let p = dir.path().join("secret");
        atomic_write(&p, b"tok", Permissions::OwnerOnly).unwrap();
        let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn default_preserves_an_existing_files_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempdir().unwrap();
        let p = dir.path().join("f");
        fs::write(&p, b"old").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        atomic_write(&p, b"new", Permissions::Default).unwrap();
        let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o640,
            "existing 0o640 must survive the replace, got {mode:o}"
        );
    }
}
