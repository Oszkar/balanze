//! Perms-preserving atomic file replace.
//!
//! One home for the subtle sequence AGENTS.md 3.4 relies on, previously
//! copy-pasted across the snapshot, statusline, settings, and OpenAI-gate
//! writers where a copy risked silently dropping the directory fsync or a
//! perms-preserving step:
//!
//! 1. resolve an existing target so a symlink itself is never replaced,
//! 2. create a unique `*.tmp` sibling in the target's directory with
//!    `O_CREAT | O_EXCL` (so two writers never share a tmp),
//! 3. write the bytes and `sync_all` the tmp (a crash between write and rename
//!    cannot lose data),
//! 4. on unix, copy the existing target's permissions onto the tmp (preserve
//!    the file's mode across the replace),
//! 5. `rename` the tmp over the target (atomic on the same filesystem),
//! 6. on unix, fsync the parent directory so the rename itself is durable.
//!
//! The tmp is removed on any failure. Windows has no portable directory fsync
//! and `std::fs::rename` does not request write-through rename semantics. The
//! replace remains atomic there, but a power loss after reported success may
//! discard the newest write. New files inherit the parent directory's ACL.
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
}

/// Atomically replace `path`'s contents with `bytes`. See the crate docs for
/// the exact sequence. The parent directory must already exist.
///
/// Returns the underlying [`io::Error`] on failure (callers map it to their own
/// error type). Failures before rename leave the target untouched and clean up
/// the tmp file. A Unix parent-directory fsync failure is returned after the
/// target has been replaced, because the new bytes are visible but the rename's
/// crash durability could not be confirmed.
pub fn atomic_write(path: &Path, bytes: &[u8], perms: Permissions) -> io::Result<()> {
    atomic_write_with_parent_sync(path, bytes, perms, sync_parent_directory)
}

fn atomic_write_with_parent_sync(
    path: &Path,
    bytes: &[u8],
    perms: Permissions,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let resolved_path = resolve_write_target(path)?;
    let path = resolved_path.as_path();
    let parent = resolve_parent(path);
    let tmp = parent.join(tmp_name(path));

    let write_result = (|| -> io::Result<()> {
        let mut f = create_tmp(&tmp, perms)?;
        // Preserve the existing target's mode before `sync_all` so the mode
        // change is durable with the bytes. A copy failure is non-fatal because
        // the freshly created tmp keeps the OS default rather than becoming
        // more permissive than its containing directory allows.
        #[cfg(unix)]
        match perms {
            Permissions::Default => {
                if let Ok(meta) = fs::metadata(path) {
                    let _ = f.set_permissions(meta.permissions());
                }
            }
        }
        f.write_all(bytes)?;
        // fsync before rename: a crash/power-loss between write and rename
        // cannot lose the bytes or the copied mode.
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    // The rename has committed at this point. Propagate a Unix directory-sync
    // failure so callers are never told the durable sequence completed when it
    // did not. The target still contains the new bytes, as documented above.
    sync_parent(parent)
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> io::Result<()> {
    // Windows has no portable directory fsync, and std's rename does not expose
    // write-through semantics. Atomicity holds, but the newest successful write
    // can be lost on power failure.
    Ok(())
}

fn create_tmp(tmp: &Path, perms: Permissions) -> io::Result<fs::File> {
    match perms {
        Permissions::Default => fs::File::create_new(tmp),
    }
}

/// Resolve an existing target before choosing its sibling tmp and rename
/// destination. This preserves a dotfile-manager symlink instead of replacing
/// the link itself with a regular file. A dangling symlink is an error: treating
/// it as a missing path would silently destroy the user's link.
/// Resolve the destination of an existing target for a read-modify-write
/// transaction. Callers that derive new bytes from the old contents must use
/// the returned path for both the read and [`atomic_write`], so a symlink
/// retarget cannot redirect publication to a different file.
///
/// Missing paths are returned unchanged so callers can create them. A dangling
/// symlink is an error rather than a missing path.
pub fn resolve_write_target(path: &Path) -> io::Result<std::path::PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(_) => fs::canonicalize(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(e) => Err(e),
    }
}

/// The directory the tmp lands in and the parent to fsync. A bare relative
/// filename has `parent() == Some("")`; normalize that (and a `None` root) to
/// `.` so the tmp is a real sibling and the parent-dir fsync opens a real
/// directory rather than the empty path (which would fail and silently drop the
/// dir-fsync from the durable sequence).
///
/// Public so callers that must `create_dir_all` the parent before writing (this
/// crate does not create directories) target exactly the directory the write
/// will use, instead of passing a raw `path.parent()` that can be `Some("")`.
pub fn resolve_parent(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
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

    #[cfg(unix)]
    fn create_symlink_or_skip(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).unwrap();
        true
    }

    #[cfg(windows)]
    fn create_symlink_or_skip(target: &Path, link: &Path) -> bool {
        match std::os::windows::fs::symlink_file(target, link) {
            Ok(()) => true,
            Err(error) if windows_symlink_privilege_missing(&error) => {
                eprintln!("skipping symlink test: Windows symlink privilege is unavailable");
                false
            }
            Err(error) => panic!("failed to create symlink fixture: {error}"),
        }
    }

    #[cfg(windows)]
    fn windows_symlink_privilege_missing(error: &io::Error) -> bool {
        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
        error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD)
    }

    #[cfg(windows)]
    #[test]
    fn only_missing_windows_symlink_privilege_is_skippable() {
        assert!(windows_symlink_privilege_missing(
            &io::Error::from_raw_os_error(1314)
        ));
        assert!(!windows_symlink_privilege_missing(
            &io::Error::from_raw_os_error(5)
        ));
    }

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

    #[cfg(any(unix, windows))]
    #[test]
    fn overwrites_symlink_target_without_replacing_the_symlink() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("managed-settings.json");
        let link = dir.path().join("settings.json");
        fs::write(&target, b"old").unwrap();
        if !create_symlink_or_skip(&target, &link) {
            return;
        }

        atomic_write(&link, b"new", Permissions::Default).unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "atomic replacement must preserve an existing symlink"
        );
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn dangling_symlink_is_rejected_without_replacing_the_link() {
        let dir = tempdir().unwrap();
        let missing_target = dir.path().join("missing-settings.json");
        let link = dir.path().join("settings.json");
        if !create_symlink_or_skip(&missing_target, &link) {
            return;
        }

        let error = atomic_write(&link, b"new", Permissions::Default).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "a dangling symlink must remain intact after a rejected write"
        );
        assert!(!missing_target.exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
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
    fn post_rename_sync_error_is_returned_with_new_bytes_visible() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("f.json");
        fs::write(&p, b"old").unwrap();
        let error = atomic_write_with_parent_sync(&p, b"new", Permissions::Default, |_| {
            Err(io::Error::other("injected parent sync failure"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "injected parent sync failure");
        assert_eq!(fs::read(&p).unwrap(), b"new");
    }

    #[test]
    fn resolve_parent_normalizes_bare_and_empty_to_dot() {
        // A bare relative filename's parent is Some(""), which must normalize to
        // "." so the parent-dir fsync targets a real directory.
        assert_eq!(resolve_parent(Path::new("bare.json")), Path::new("."));
        assert_eq!(resolve_parent(Path::new("dir/f.json")), Path::new("dir"));
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
