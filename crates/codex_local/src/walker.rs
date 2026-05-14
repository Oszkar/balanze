//! Walks `~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-*.jsonl` to find
//! the latest session file by mtime.
//!
//! Codex CLI nests session files by date (year / month / day), unlike
//! Claude Code which nests by project slug. The walker recurses
//! arbitrarily deep so future Codex CLI versions that reorganize the
//! tree don't immediately break us — anything matching the
//! `rollout-*.jsonl` filename pattern is considered a session file.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::errors::ParseError;

/// Environment variable that overrides the default `~/.codex/sessions/`
/// path. Mirrors the `CLAUDE_CONFIG_DIR` pattern from ccusage. Accepts
/// a single path; future versions may accept comma-separated multi-path
/// if user demand surfaces (none observed in the field as of v0.1).
pub const CODEX_CONFIG_DIR_ENV: &str = "CODEX_CONFIG_DIR";

/// Resolve the directory holding Codex session JSONL files.
///
/// Resolution order:
/// 1. `CODEX_CONFIG_DIR` env var (if set and non-empty), interpreted
///    as the path to `sessions/`'s **parent** — so the function appends
///    `sessions` to it. (Matches Codex CLI's own `$CODEX_HOME` semantic.)
/// 2. `~/.codex/sessions/` via `directories::UserDirs::home_dir()`.
///
/// Read via `env::var_os` rather than `env::var` so non-UTF-8 path
/// values on Unix don't silently fall back to the default (matches the
/// `CLAUDE_CONFIG_DIR` handling in `crates/claude_parser/src/walker.rs`).
///
/// Returns `Err(FileMissing)` if the resolved directory doesn't exist.
/// Caller maps this to a "Codex not installed" UI state and continues
/// with the other matrix cells populated.
pub fn find_codex_sessions_dir() -> Result<PathBuf, ParseError> {
    // 1. Env-var override (var_os for non-UTF-8 path safety on Unix).
    if let Some(raw) = std::env::var_os(CODEX_CONFIG_DIR_ENV) {
        if !raw.is_empty() {
            let candidate = PathBuf::from(&raw).join("sessions");
            if candidate.is_dir() {
                return Ok(candidate);
            }
            return Err(ParseError::FileMissing(candidate));
        }
    }

    // 2. Default: ~/.codex/sessions/ via UserDirs (NOT ProjectDirs —
    //    those are for Balanze's own app data, not Codex's user data).
    let user = directories::UserDirs::new()
        .ok_or_else(|| ParseError::FileMissing(PathBuf::from("$HOME (unresolvable)")))?;
    let path = user.home_dir().join(".codex").join("sessions");
    if path.is_dir() {
        Ok(path)
    } else {
        Err(ParseError::FileMissing(path))
    }
}

/// Recursively walk `root` for `rollout-*.jsonl` files and return the
/// path with the latest mtime, or `Ok(None)` if no session files exist
/// (e.g. Codex installed but never run).
///
/// # Error policy
///
/// - **Root failure is loud.** If `root` doesn't exist returns
///   `Err(FileMissing)`. If `read_dir` on `root` fails (e.g. permission
///   denied on the user's `~/.codex/`), returns `Err(IoError)`.
/// - **Per-subdirectory failure is loud too.** If `read_dir` on any
///   descendant fails, the whole walk returns `Err(IoError)` rather
///   than partial results — a single unreadable subtree shouldn't
///   silently produce a stale "latest" file from elsewhere.
/// - **Per-entry failure is best-effort.** Entries we can't stat (e.g.
///   dangling symlinks, transient races where a file is removed
///   between `read_dir` and `metadata`) are skipped. The walker logs
///   nothing (per AGENTS.md §3.2 "no logging above debug for pure
///   data crates") but the choice is intentional: a single unreadable
///   entry should not break the whole snapshot.
///
/// Walks arbitrarily deep — the YYYY/MM/DD nesting Codex CLI uses
/// today is not assumed; any depth works.
pub fn find_latest_session(root: &Path) -> Result<Option<PathBuf>, ParseError> {
    if !root.exists() {
        return Err(ParseError::FileMissing(root.to_path_buf()));
    }
    let mut best: Option<(SystemTime, PathBuf)> = None;
    walk(root, &mut best)?;
    Ok(best.map(|(_, p)| p))
}

fn walk(dir: &Path, best: &mut Option<(SystemTime, PathBuf)>) -> Result<(), ParseError> {
    let entries = std::fs::read_dir(dir).map_err(|source| ParseError::IoError {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry_result in entries {
        // Per-entry DirEntry failure is best-effort skip — see the
        // function's doc-comment "Per-entry failure" clause. read_dir
        // errors on the whole directory already propagated above.
        let entry = match entry_result {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue, // best-effort skip; see doc-comment
        };
        if metadata.is_dir() {
            walk(&path, best)?; // subtree IO errors propagate (loud)
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        // Filter on filename pattern: rollout-*.jsonl. Anything else
        // (legacy formats, swap files, IDE backup files) is skipped.
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
            continue;
        }
        let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        match best {
            Some((best_mtime, _)) if *best_mtime >= mtime => {}
            _ => *best = Some((mtime, path.clone())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use std::time::Duration;

    use tempfile::TempDir;

    /// Serializes tests that mutate `CODEX_CONFIG_DIR`. Rust's test
    /// harness runs tests within a single binary concurrently by
    /// default, and `set_var` / `remove_var` are process-global —
    /// without this gate, the two env-touching tests can race and
    /// observe each other's writes.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn touch_jsonl(path: &Path, content: &str, mtime_offset_secs: i64) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
        // Bump mtime by reopening with write access (set_modified on
        // Windows requires FILE_WRITE_ATTRIBUTES; a read-only handle
        // gets ACCESS_DENIED).
        let now = SystemTime::now();
        let target = if mtime_offset_secs >= 0 {
            now + Duration::from_secs(mtime_offset_secs as u64)
        } else {
            now - Duration::from_secs((-mtime_offset_secs) as u64)
        };
        let f = fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(target).unwrap();
    }

    #[test]
    fn find_codex_sessions_dir_honors_env_var() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        std::env::set_var(CODEX_CONFIG_DIR_ENV, tmp.path());
        let resolved = find_codex_sessions_dir().unwrap();
        std::env::remove_var(CODEX_CONFIG_DIR_ENV);
        assert_eq!(resolved, sessions);
    }

    #[test]
    fn find_codex_sessions_dir_env_var_missing_path_errors() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let nonexistent = tmp.path().join("does-not-exist");
        std::env::set_var(CODEX_CONFIG_DIR_ENV, &nonexistent);
        let result = find_codex_sessions_dir();
        std::env::remove_var(CODEX_CONFIG_DIR_ENV);
        match result {
            Err(ParseError::FileMissing(p)) => {
                assert!(p.to_string_lossy().contains("does-not-exist"));
                assert!(p.ends_with("sessions"));
            }
            other => panic!("expected FileMissing, got {other:?}"),
        }
    }

    #[test]
    fn find_latest_session_walks_three_levels_and_picks_newest() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Older file
        touch_jsonl(
            &root.join("2026/05/14/rollout-old.jsonl"),
            "{}",
            -3600, // 1h ago
        );
        // Newer file (deeper in the tree)
        touch_jsonl(
            &root.join("2026/05/15/rollout-new.jsonl"),
            "{}",
            -60, // 1min ago
        );
        // Non-rollout file (must be ignored)
        touch_jsonl(&root.join("2026/05/15/random.jsonl"), "{}", 0);
        // Wrong extension (must be ignored)
        touch_jsonl(&root.join("2026/05/15/rollout-foo.txt"), "{}", 0);

        let latest = find_latest_session(root).unwrap().expect("non-empty");
        assert!(
            latest.ends_with("rollout-new.jsonl"),
            "got {}",
            latest.display()
        );
    }

    #[test]
    fn find_latest_session_empty_dir_returns_none() {
        let tmp = TempDir::new().unwrap();
        let result = find_latest_session(tmp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn find_latest_session_missing_dir_errors() {
        let tmp = TempDir::new().unwrap();
        let nonexistent = tmp.path().join("nope");
        let result = find_latest_session(&nonexistent);
        match result {
            Err(ParseError::FileMissing(p)) => assert_eq!(p, nonexistent),
            other => panic!("expected FileMissing, got {other:?}"),
        }
    }
}
