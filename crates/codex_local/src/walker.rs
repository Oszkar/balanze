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
/// # Freshness semantics (intentional)
///
/// This function is **age-agnostic**: it returns the newest-mtime
/// rollout file regardless of how old it is. A user who hasn't run
/// Codex in a week still gets back their last session file, and a user
/// who opens Balanze at 00:00:30 local (before today's
/// `YYYY/MM/DD/` directory has been created) gets back yesterday's
/// last session — which IS the latest data Codex has produced.
///
/// Callers that need a freshness gate should compare
/// [`CodexQuotaSnapshot::observed_at`](crate::CodexQuotaSnapshot::observed_at)
/// (the Codex CLI's own timestamp on the rate-limit event) to wall-clock
/// time. The walker deliberately does not filter, because:
/// - Codex's `primary` rate-limit window is 7 days, so a 6-day-old
///   snapshot is still semantically valid.
/// - Hiding stale data is a renderer-policy concern, not a parser one
///   — the snapshot crate (`codex_local`) sits below the renderer in
///   the dependency graph and should expose the signal, not gate it.
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

/// Collect every `rollout-*.jsonl` under `root`, sorted by mtime descending
/// (newest first). Same error policy as [`find_latest_session`]: root-level
/// or subtree `read_dir` failures propagate as `IoError`; per-entry stat
/// failures are best-effort-skipped.
///
/// Used by [`crate::read_codex_quota`] to walk older sessions when a fresh
/// rollout file has no `token_count` event yet (e.g. just-created session
/// at a day-rollover). The pure-walker shape keeps the boundary that
/// `codex_local` is the only reader of `~/.codex/` (AGENTS.md §4 #11).
pub fn collect_sessions_newest_first(root: &Path) -> Result<Vec<PathBuf>, ParseError> {
    if !root.exists() {
        return Err(ParseError::FileMissing(root.to_path_buf()));
    }
    let mut all: Vec<(SystemTime, PathBuf)> = Vec::new();
    walk_all(root, &mut all)?;
    // Descending by mtime; PathBuf tiebreaker keeps the order deterministic
    // when two files share an mtime (filesystem mtime resolution varies).
    all.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Ok(all.into_iter().map(|(_, p)| p).collect())
}

fn walk_all(dir: &Path, out: &mut Vec<(SystemTime, PathBuf)>) -> Result<(), ParseError> {
    let entries = std::fs::read_dir(dir).map_err(|source| ParseError::IoError {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry_result in entries {
        let entry = match entry_result {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if metadata.is_dir() {
            walk_all(&path, out)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
            continue;
        }
        let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        out.push((mtime, path));
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

    #[test]
    fn collect_sessions_newest_first_orders_by_mtime_descending() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        touch_jsonl(&root.join("a/rollout-old.jsonl"), "{}", -3600); // 1h ago
        touch_jsonl(&root.join("b/rollout-new.jsonl"), "{}", -60); // 1min ago
        touch_jsonl(&root.join("c/rollout-mid.jsonl"), "{}", -1800); // 30min ago
                                                                     // Non-matching files must be ignored.
        touch_jsonl(&root.join("c/random.jsonl"), "{}", -10);
        touch_jsonl(&root.join("c/rollout-foo.txt"), "{}", -10);

        let sessions = collect_sessions_newest_first(root).unwrap();
        assert_eq!(sessions.len(), 3);
        assert!(
            sessions[0].ends_with("rollout-new.jsonl"),
            "newest first: {}",
            sessions[0].display()
        );
        assert!(sessions[1].ends_with("rollout-mid.jsonl"));
        assert!(sessions[2].ends_with("rollout-old.jsonl"));
    }

    #[test]
    fn collect_sessions_newest_first_empty_dir_returns_empty_vec() {
        let tmp = TempDir::new().unwrap();
        let sessions = collect_sessions_newest_first(tmp.path()).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn collect_sessions_newest_first_missing_dir_errors() {
        let tmp = TempDir::new().unwrap();
        let nonexistent = tmp.path().join("nope");
        match collect_sessions_newest_first(&nonexistent) {
            Err(ParseError::FileMissing(p)) => assert_eq!(p, nonexistent),
            other => panic!("expected FileMissing, got {other:?}"),
        }
    }

    #[test]
    fn read_codex_quota_falls_back_to_older_session_when_newest_has_no_token_count() {
        // Regression: read_codex_quota previously parsed only the newest
        // rollout file. A fresh day-rollover / freshly-spawned session that
        // hasn't logged a `token_count` yet would return Ok(None) and hide
        // a still-valid older snapshot whose 7-day window is unexpired.
        let _guard = ENV_MUTEX.lock().unwrap();

        const SESSION_META: &str = r#"{"timestamp":"2026-05-14T06:23:20.076Z","type":"session_meta","payload":{"id":"00000000-0000-7000-8000-000000000001","timestamp":"2026-05-14T06:23:10.584Z","cwd":"E:\\test","originator":"codex_exec","cli_version":"0.130.0"}}"#;
        const TOKEN_COUNT_3PCT: &str = r#"{"timestamp":"2026-05-14T06:23:25.393Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":29331}},"rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":3.0,"window_minutes":10080,"resets_at":1779344602},"secondary":null,"credits":null,"plan_type":"go","rate_limit_reached_type":null}}}"#;

        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();

        // Older session: has a valid token_count → semantic Ok(Some).
        let older = sessions.join("2026/05/14/rollout-older.jsonl");
        let older_content = format!("{SESSION_META}\n{TOKEN_COUNT_3PCT}\n");
        touch_jsonl(&older, &older_content, -3600); // 1h ago

        // Newer session: session_meta only (no token_count yet) → Ok(None).
        let newer = sessions.join("2026/05/15/rollout-empty.jsonl");
        let newer_content = format!("{SESSION_META}\n");
        touch_jsonl(&newer, &newer_content, -60); // 1min ago

        std::env::set_var(CODEX_CONFIG_DIR_ENV, tmp.path());
        let result = crate::read_codex_quota();
        std::env::remove_var(CODEX_CONFIG_DIR_ENV);

        let snap = result
            .unwrap()
            .expect("expected the older session's snapshot");
        assert!(
            (snap.primary.used_percent - 3.0).abs() < 1e-9,
            "got used_percent {}",
            snap.primary.used_percent
        );
    }

    #[test]
    fn read_codex_quota_returns_none_when_all_sessions_lack_token_count() {
        let _guard = ENV_MUTEX.lock().unwrap();

        const SESSION_META: &str = r#"{"timestamp":"2026-05-14T06:23:20.076Z","type":"session_meta","payload":{"id":"00000000-0000-7000-8000-000000000001","timestamp":"2026-05-14T06:23:10.584Z","cwd":"E:\\test","originator":"codex_exec","cli_version":"0.130.0"}}"#;

        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        touch_jsonl(
            &sessions.join("2026/05/15/rollout-a.jsonl"),
            &format!("{SESSION_META}\n"),
            -60,
        );
        touch_jsonl(
            &sessions.join("2026/05/14/rollout-b.jsonl"),
            &format!("{SESSION_META}\n"),
            -3600,
        );

        std::env::set_var(CODEX_CONFIG_DIR_ENV, tmp.path());
        let result = crate::read_codex_quota();
        std::env::remove_var(CODEX_CONFIG_DIR_ENV);

        assert!(
            result.unwrap().is_none(),
            "all sessions empty → Ok(None) (not an error)"
        );
    }
}
