use std::fs;
use std::path::{Path, PathBuf};

use crate::types::ParseError;

/// Build the ordered candidate list for the Claude Code projects directory.
///
/// Search order matches ccusage / Claude Code itself:
///   1. `$XDG_CONFIG_HOME/claude/projects` (only when `xdg` is `Some` and non-empty)
///   2. `<home>/.claude/projects`            — the most common case
///   3. `<home>/.config/claude/projects`     — newer Claude Code installs on Linux
fn candidate_claude_projects_dirs_in(home: &Path, xdg: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::with_capacity(3);
    if let Some(x) = xdg {
        if !x.as_os_str().is_empty() {
            candidates.push(x.join("claude").join("projects"));
        }
    }
    candidates.push(home.join(".claude").join("projects"));
    candidates.push(home.join(".config").join("claude").join("projects"));
    candidates
}

fn home_dir_from_env() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// The ordered list of paths `find_claude_projects_dir` will check.
///
/// Reads `$USERPROFILE` / `$HOME` and `$XDG_CONFIG_HOME` from the environment.
/// Returned vec is empty only if no home variable is set at all (which on a
/// real install would already be broken for many other reasons).
pub fn candidate_claude_projects_dirs() -> Vec<PathBuf> {
    let Some(home) = home_dir_from_env() else {
        return Vec::new();
    };
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    candidate_claude_projects_dirs_in(&home, xdg.as_deref())
}

/// Locate the Claude Code projects directory on disk.
///
/// On a typical install this is `~/.claude/projects`. Linux installs using
/// `$XDG_CONFIG_HOME` are supported, as are newer installs that moved the
/// directory under `~/.config/claude/projects`.
///
/// Returns the first candidate from [`candidate_claude_projects_dirs`] that
/// exists. If none exist, returns `ParseError::FileMissing` with the canonical
/// `<home>/.claude/projects` path (the expected default — XDG / `.config`
/// candidates are fallbacks, not the default).
pub fn find_claude_projects_dir() -> Result<PathBuf, ParseError> {
    let candidates = candidate_claude_projects_dirs();
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    let dot_claude_suffix = Path::new(".claude").join("projects");
    let primary = candidates
        .iter()
        .find(|p| p.ends_with(&dot_claude_suffix))
        .cloned()
        .or_else(|| candidates.into_iter().next())
        .unwrap_or_else(|| PathBuf::from(".claude/projects"));
    Err(ParseError::FileMissing(primary))
}

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
        assert_eq!(
            results.len(),
            3,
            "expected exactly 3 jsonl files; got {names:?}"
        );
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

    // --- candidate_claude_projects_dirs_in: pure helper, no env reads ---

    #[test]
    fn candidates_xdg_present_is_first() {
        let home = Path::new("/home/alice");
        let xdg = Path::new("/cfg");
        let got = candidate_claude_projects_dirs_in(home, Some(xdg));
        assert_eq!(
            got,
            vec![
                PathBuf::from("/cfg/claude/projects"),
                PathBuf::from("/home/alice/.claude/projects"),
                PathBuf::from("/home/alice/.config/claude/projects"),
            ]
        );
    }

    #[test]
    fn candidates_xdg_absent_returns_home_pair() {
        let home = Path::new("/home/alice");
        let got = candidate_claude_projects_dirs_in(home, None);
        assert_eq!(
            got,
            vec![
                PathBuf::from("/home/alice/.claude/projects"),
                PathBuf::from("/home/alice/.config/claude/projects"),
            ]
        );
    }

    #[test]
    fn candidates_xdg_empty_string_is_ignored() {
        let home = Path::new("/home/alice");
        let xdg = Path::new("");
        let got = candidate_claude_projects_dirs_in(home, Some(xdg));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], PathBuf::from("/home/alice/.claude/projects"));
    }

    #[test]
    fn finds_dot_claude_when_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let dot_claude = dir.path().join(".claude").join("projects");
        fs::create_dir_all(&dot_claude).unwrap();
        let candidates = candidate_claude_projects_dirs_in(dir.path(), None);
        let found = candidates.iter().find(|p| p.exists()).cloned();
        assert_eq!(found, Some(dot_claude));
    }

    #[test]
    fn finds_dot_config_claude_when_only_that_exists() {
        let dir = tempfile::tempdir().unwrap();
        let dot_config = dir.path().join(".config").join("claude").join("projects");
        fs::create_dir_all(&dot_config).unwrap();
        let candidates = candidate_claude_projects_dirs_in(dir.path(), None);
        let found = candidates.iter().find(|p| p.exists()).cloned();
        assert_eq!(found, Some(dot_config));
    }

    #[test]
    fn prefers_xdg_over_dot_claude_when_both_exist() {
        let dir = tempfile::tempdir().unwrap();
        let xdg_root = dir.path().join("cfg");
        let xdg_projects = xdg_root.join("claude").join("projects");
        let dot_claude = dir.path().join(".claude").join("projects");
        fs::create_dir_all(&xdg_projects).unwrap();
        fs::create_dir_all(&dot_claude).unwrap();
        let candidates = candidate_claude_projects_dirs_in(dir.path(), Some(&xdg_root));
        let found = candidates.iter().find(|p| p.exists()).cloned();
        assert_eq!(found, Some(xdg_projects));
    }
}
