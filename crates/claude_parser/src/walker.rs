use std::fs;
use std::path::{Path, PathBuf};

use crate::types::ParseError;

/// Recursively find all `*.jsonl` files under `root`.
///
/// Returned paths are sorted by modification time, newest first — callers
/// that want recent events can stop early. Subagent JSONLs (under
/// `<session>/subagents/agent-*.jsonl`) are included; they carry the same
/// schema as the parent session.
///
/// Returns `FileMissing` if `root` does not exist, `PermissionDenied` if a
/// directory in the walk cannot be opened, `IoError` for other I/O failures.
pub fn find_jsonl_files(root: impl AsRef<Path>) -> Result<Vec<PathBuf>, ParseError> {
    let root = root.as_ref();
    if !root.exists() {
        return Err(ParseError::FileMissing(root.to_path_buf()));
    }
    let mut results = Vec::new();
    walk_into(root, &mut results)?;
    results.sort_by(|a, b| {
        let mtime_a = a.metadata().and_then(|m| m.modified()).ok();
        let mtime_b = b.metadata().and_then(|m| m.modified()).ok();
        mtime_b.cmp(&mtime_a)
    });
    Ok(results)
}

fn walk_into(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), ParseError> {
    let entries = fs::read_dir(dir).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => ParseError::PermissionDenied(dir.to_path_buf()),
        _ => ParseError::IoError {
            path: dir.to_path_buf(),
            source: e,
        },
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| ParseError::IoError {
            path: dir.to_path_buf(),
            source: e,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| ParseError::IoError {
            path: path.clone(),
            source: e,
        })?;
        if file_type.is_dir() {
            walk_into(&path, out)?;
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};

    #[test]
    fn finds_jsonl_files_recursively_skipping_other_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let session = root.join("E--Programming-proj").join("session-uuid");
        fs::create_dir_all(&session).unwrap();
        File::create(session.join("main.jsonl")).unwrap();
        File::create(session.join("ignore.txt")).unwrap();
        File::create(session.join("backup.jsonl.bak")).unwrap();

        let subagents = session.join("subagents");
        fs::create_dir_all(&subagents).unwrap();
        File::create(subagents.join("agent-abc.jsonl")).unwrap();
        File::create(subagents.join("agent-acompact-def.jsonl")).unwrap();

        let results = find_jsonl_files(root).unwrap();
        let names: Vec<_> = results
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(results.len(), 3, "expected exactly 3 jsonl files; got {names:?}");
        assert!(names.iter().any(|n| n == "main.jsonl"));
        assert!(names.iter().any(|n| n == "agent-abc.jsonl"));
        assert!(names.iter().any(|n| n == "agent-acompact-def.jsonl"));
    }

    #[test]
    fn missing_root_returns_file_missing_error() {
        let nonexistent = std::env::temp_dir().join("balanze-test-does-not-exist-xyzzy");
        // Ensure it doesn't exist (paranoia for a flaky CI environment).
        let _ = fs::remove_dir_all(&nonexistent);

        match find_jsonl_files(&nonexistent) {
            Err(ParseError::FileMissing(p)) => assert_eq!(p, nonexistent),
            other => panic!("expected FileMissing, got {other:?}"),
        }
    }

    #[test]
    fn empty_dir_returns_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let results = find_jsonl_files(dir.path()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn results_sorted_newest_first_by_mtime() {
        use std::thread::sleep;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        File::create(root.join("a.jsonl")).unwrap();
        // Filesystems have coarse mtime resolution; sleep enough to guarantee
        // distinguishable timestamps across platforms.
        sleep(Duration::from_millis(50));
        File::create(root.join("b.jsonl")).unwrap();
        sleep(Duration::from_millis(50));
        File::create(root.join("c.jsonl")).unwrap();

        let results = find_jsonl_files(root).unwrap();
        let names: Vec<_> = results
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["c.jsonl", "b.jsonl", "a.jsonl"]);
    }
}
