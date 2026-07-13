//! Pure, network-free cache for the self-composed OpenAI cost figure.
//!
//! One global entry per machine (NOT per transcript): OpenAI costs are
//! account-wide, and AGENTS.md 3.1 requires the billing fetch be gated to at
//! most once per 300s machine-wide. The entry is invalidated by a fingerprint
//! of the resolved OpenAI key, so a key rotation forces a refetch and distinct
//! keys never share a value. The fingerprint is a hash, never the key itself
//! (3.4 secret hygiene). The 300s TTL IS the 3.1 politeness gate; the failure
//! cooldown keeps a broken API from being retried every turn.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// TTL for a cached OpenAI cost figure. This is the AGENTS.md 3.1 5-minute gate.
pub const OPENAI_TTL_SECS: i64 = 300;
/// After a failed fetch, do not retry for this long. Matches the TTL: AGENTS.md
/// 3.1 caps OpenAI billing polls at one per 5 minutes regardless of outcome, so
/// a failing endpoint (bad key, 429, 5xx, timeout) must not be re-polled every
/// minute inside the gate.
pub const NEGATIVE_COOLDOWN_SECS: i64 = 300;

const FILE_NAME: &str = "openai-cost.json";
const LEASE_FILE_NAME: &str = "openai-cost.refresh.lease";
const REFRESH_LEASE_STALE_AFTER: StdDuration = StdDuration::from_secs(10);
static LEASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Result of attempting to become the single process allowed to refresh the
/// OpenAI cache. `Busy` is not an error: the caller should serve stale data or
/// wait for the current owner for a bounded period.
pub(crate) enum LeaseAttempt {
    Acquired(RefreshLease),
    Busy,
}

/// A dependency-free interprocess lease backed by `create_new`. Cleanup is
/// token-checked so an old owner can never remove a successor's lease after a
/// stale-lock takeover.
pub(crate) struct RefreshLease {
    path: PathBuf,
    token: String,
}

impl Drop for RefreshLease {
    fn drop(&mut self) {
        match std::fs::read_to_string(&self.path) {
            Ok(current) if current == self.token => {
                if let Err(error) = std::fs::remove_file(&self.path) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        tracing::debug!("statusline cache lease cleanup failed: {error}");
                    }
                }
            }
            Ok(_) | Err(_) => {
                // Missing or replaced: a stale owner must not remove the
                // successor lease now occupying the canonical path.
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAiCostEntry {
    /// FNV-1a hex of the resolved OpenAI key; a mismatch invalidates the entry.
    pub fingerprint: String,
    /// Last successfully fetched total, micro-USD. `None` if only failures so far.
    pub total_micro_usd: Option<i64>,
    /// When `total_micro_usd` was last fetched successfully.
    pub fetched_at: Option<DateTime<Utc>>,
    /// When the most recent fetch attempt failed (drives the negative cooldown).
    pub last_failure_at: Option<DateTime<Utc>>,
}

/// `<BALANZE_CACHE_DIR_OVERRIDE or ProjectDirs.cache>/statusline`.
pub fn cache_dir_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("BALANZE_CACHE_DIR_OVERRIDE") {
        return Some(PathBuf::from(dir).join("statusline"));
    }
    directories::ProjectDirs::from("me", "oszkar", "Balanze")
        .map(|d| d.cache_dir().join("statusline"))
}

/// Stable FNV-1a-64 hex of the resolved key (empty string when no key). Never
/// the key itself - this is written to disk, so it must not be reversible.
pub fn key_fingerprint(key: Option<&str>) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.unwrap_or("").as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Read the entry iff present, parseable, and its fingerprint matches.
pub fn read(dir: &Path, fingerprint: &str) -> Option<OpenAiCostEntry> {
    let bytes = std::fs::read(dir.join(FILE_NAME)).ok()?;
    let entry: OpenAiCostEntry = serde_json::from_slice(&bytes).ok()?;
    (entry.fingerprint == fingerprint).then_some(entry)
}

pub fn is_fresh(entry: &OpenAiCostEntry, now: DateTime<Utc>) -> bool {
    entry.total_micro_usd.is_some()
        && entry
            .fetched_at
            .is_some_and(|t| now.signed_duration_since(t).num_seconds() < OPENAI_TTL_SECS)
}

pub fn in_cooldown(entry: &OpenAiCostEntry, now: DateTime<Utc>) -> bool {
    entry
        .last_failure_at
        .is_some_and(|t| now.signed_duration_since(t).num_seconds() < NEGATIVE_COOLDOWN_SECS)
}

/// Try to acquire the machine-wide OpenAI refresh lease. A lease older than
/// the maximum 3-second HTTP request window plus scheduling margin is treated
/// as abandoned and removed before one retry. Ordinary live contention returns
/// `Busy` immediately. A process suspended beyond that 10-second expiry has
/// forfeited ownership and may overlap its successor after resuming; that is
/// the deliberate stale-recovery boundary of this portable file lease.
pub(crate) fn try_acquire_refresh_lease(dir: &Path) -> std::io::Result<LeaseAttempt> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(LEASE_FILE_NAME);

    for _ in 0..2 {
        let token = lease_token();
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                if let Err(error) = file
                    .write_all(token.as_bytes())
                    .and_then(|()| file.sync_all())
                {
                    let _ = std::fs::remove_file(&path);
                    return Err(error);
                }
                return Ok(LeaseAttempt::Acquired(RefreshLease { path, token }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !lease_is_stale(&path, SystemTime::now()) {
                    return Ok(LeaseAttempt::Busy);
                }
                match std::fs::remove_file(&path) {
                    Ok(()) => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }

    Ok(LeaseAttempt::Busy)
}

fn lease_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = LEASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{nanos}-{sequence}", std::process::id())
}

fn lease_is_stale(path: &Path, now: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age >= REFRESH_LEASE_STALE_AFTER)
}

pub fn write_success(dir: &Path, fingerprint: &str, total_micro_usd: i64, now: DateTime<Utc>) {
    write(
        dir,
        &OpenAiCostEntry {
            fingerprint: fingerprint.to_string(),
            total_micro_usd: Some(total_micro_usd),
            fetched_at: Some(now),
            last_failure_at: None,
        },
    );
}

pub fn write_failure(dir: &Path, fingerprint: &str, now: DateTime<Utc>) {
    let prior = read(dir, fingerprint);
    write(
        dir,
        &OpenAiCostEntry {
            fingerprint: fingerprint.to_string(),
            total_micro_usd: prior.as_ref().and_then(|e| e.total_micro_usd),
            fetched_at: prior.as_ref().and_then(|e| e.fetched_at),
            last_failure_at: Some(now),
        },
    );
}

/// Best-effort durable atomic write. Errors are logged at debug and swallowed:
/// a cache write failure must never break the statusline.
fn write(dir: &Path, entry: &OpenAiCostEntry) {
    if let Err(e) = try_write(dir, entry) {
        tracing::debug!("statusline cache write failed: {e}");
    }
}

fn try_write(dir: &Path, entry: &OpenAiCostEntry) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let final_path = dir.join(FILE_NAME);
    let bytes = serde_json::to_vec(entry).map_err(std::io::Error::other)?;
    atomic_file::atomic_write(&final_path, &bytes, atomic_file::Permissions::Default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone as _, Utc};
    use tempfile::tempdir;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn t0() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).unwrap()
    }

    #[test]
    fn read_missing_is_none() {
        let dir = tempdir().unwrap();
        assert!(read(dir.path(), "fp").is_none());
    }

    #[test]
    fn write_success_then_read_roundtrips_and_is_fresh() {
        let dir = tempdir().unwrap();
        write_success(dir.path(), "fp", 4_200_000, t0());
        let e = read(dir.path(), "fp").expect("entry");
        assert_eq!(e.total_micro_usd, Some(4_200_000));
        assert!(is_fresh(&e, t0() + Duration::seconds(299)));
        assert!(
            !is_fresh(&e, t0() + Duration::seconds(300)),
            "exactly TTL is not fresh (< not <=)"
        );
        assert!(!is_fresh(&e, t0() + Duration::seconds(301)));
        // no leftover tmp file
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|d| d.ok())
            .filter(|d| d.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no .tmp left");
    }

    #[test]
    fn fingerprint_mismatch_reads_none() {
        let dir = tempdir().unwrap();
        write_success(dir.path(), "fp-a", 1, t0());
        assert!(read(dir.path(), "fp-b").is_none());
    }

    #[test]
    fn write_failure_preserves_value_sets_cooldown() {
        let dir = tempdir().unwrap();
        write_success(dir.path(), "fp", 500, t0());
        write_failure(dir.path(), "fp", t0() + Duration::seconds(400));
        let e = read(dir.path(), "fp").expect("entry");
        assert_eq!(e.total_micro_usd, Some(500), "prior value kept");
        assert!(in_cooldown(&e, t0() + Duration::seconds(401)));
        // 100s after the failure is still inside the 300s cooldown (the 3.1 gate).
        assert!(in_cooldown(&e, t0() + Duration::seconds(500)));
        // 301s after the failure the cooldown has elapsed.
        assert!(!in_cooldown(&e, t0() + Duration::seconds(701)));
        assert!(!is_fresh(&e, t0() + Duration::seconds(401)), "stale by TTL");
    }

    #[test]
    fn write_failure_without_prior_has_no_value() {
        let dir = tempdir().unwrap();
        write_failure(dir.path(), "fp", t0());
        let e = read(dir.path(), "fp").expect("entry");
        assert_eq!(e.total_micro_usd, None);
        assert!(!is_fresh(&e, t0()));
        assert!(in_cooldown(&e, t0()));
        assert!(in_cooldown(&e, t0() + Duration::seconds(299)));
        assert!(
            !in_cooldown(&e, t0() + Duration::seconds(300)),
            "exactly the cooldown window is not in cooldown (< not <=)"
        );
    }

    #[test]
    fn fingerprint_is_stable_and_distinguishes_keys() {
        assert_eq!(
            key_fingerprint(Some("sk-abc")),
            key_fingerprint(Some("sk-abc"))
        );
        assert_ne!(
            key_fingerprint(Some("sk-abc")),
            key_fingerprint(Some("sk-xyz"))
        );
        assert_eq!(key_fingerprint(None), key_fingerprint(Some("")));
        // never the raw key
        assert!(!key_fingerprint(Some("sk-abc")).contains("sk-abc"));
    }

    #[test]
    fn cache_dir_path_honors_override() {
        // Serialize env mutation across every env-touching test in this module.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        // SAFETY: ENV_LOCK serializes env-mutating tests in this module; restore after.
        unsafe { std::env::set_var("BALANZE_CACHE_DIR_OVERRIDE", dir.path()) };
        let p = cache_dir_path().expect("path");
        assert_eq!(p, dir.path().join("statusline"));
        unsafe { std::env::remove_var("BALANZE_CACHE_DIR_OVERRIDE") };
    }

    #[test]
    fn live_lease_excludes_a_second_owner() {
        let dir = tempdir().unwrap();
        let first = match try_acquire_refresh_lease(dir.path()).unwrap() {
            LeaseAttempt::Acquired(lease) => lease,
            LeaseAttempt::Busy => panic!("first lease must be acquired"),
        };
        assert!(matches!(
            try_acquire_refresh_lease(dir.path()).unwrap(),
            LeaseAttempt::Busy
        ));

        drop(first);
        assert!(matches!(
            try_acquire_refresh_lease(dir.path()).unwrap(),
            LeaseAttempt::Acquired(_)
        ));
    }

    #[test]
    fn abandoned_lease_recovers_and_old_owner_cannot_remove_successor() {
        let dir = tempdir().unwrap();
        let old = match try_acquire_refresh_lease(dir.path()).unwrap() {
            LeaseAttempt::Acquired(lease) => lease,
            LeaseAttempt::Busy => panic!("first lease must be acquired"),
        };
        let lease_path = dir.path().join(LEASE_FILE_NAME);
        let file = OpenOptions::new().write(true).open(&lease_path).unwrap();
        file.set_modified(
            SystemTime::now() - REFRESH_LEASE_STALE_AFTER - StdDuration::from_secs(1),
        )
        .unwrap();

        let successor = match try_acquire_refresh_lease(dir.path()).unwrap() {
            LeaseAttempt::Acquired(lease) => lease,
            LeaseAttempt::Busy => panic!("stale lease must be recovered"),
        };
        drop(old);
        assert!(
            lease_path.exists(),
            "old owner must preserve successor lease"
        );
        drop(successor);
        assert!(!lease_path.exists(), "successor cleans up its own lease");
    }

    #[test]
    fn concurrent_cache_publication_is_always_valid_json() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempdir().unwrap();
        write_success(dir.path(), "fp", 0, t0());
        let running = Arc::new(AtomicUsize::new(4));
        let mut writers = Vec::new();
        for writer_id in 0..4 {
            let path = dir.path().to_path_buf();
            let running = Arc::clone(&running);
            writers.push(std::thread::spawn(move || {
                for value in 0..100 {
                    write_success(&path, "fp", writer_id * 100 + value, t0());
                }
                running.fetch_sub(1, Ordering::AcqRel);
            }));
        }

        while running.load(Ordering::Acquire) != 0 {
            let bytes = std::fs::read(dir.path().join(FILE_NAME)).unwrap();
            let entry: OpenAiCostEntry = serde_json::from_slice(&bytes)
                .expect("atomic publication must expose a complete JSON document");
            assert_eq!(entry.fingerprint, "fp");
        }
        for writer in writers {
            writer.join().unwrap();
        }
        assert!(read(dir.path(), "fp").is_some());
    }

    #[test]
    fn refresh_lease_is_exclusive_across_processes() {
        let dir = tempdir().unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for child_id in 0..2 {
            children.push(
                std::process::Command::new(&executable)
                    .args([
                        "--exact",
                        "cache::tests::refresh_lease_process_helper",
                        "--nocapture",
                    ])
                    .env("BALANZE_LEASE_TEST_DIR", dir.path())
                    .env("BALANZE_LEASE_TEST_CHILD", child_id.to_string())
                    .spawn()
                    .unwrap(),
            );
        }

        wait_for_test_file(&dir.path().join("ready-0"));
        wait_for_test_file(&dir.path().join("ready-1"));
        std::fs::write(dir.path().join("start"), b"go").unwrap();
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }

        let outcomes = [
            std::fs::read_to_string(dir.path().join("result-0")).unwrap(),
            std::fs::read_to_string(dir.path().join("result-1")).unwrap(),
        ];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| *outcome == "acquired")
                .count(),
            1,
            "exactly one process may own the refresh lease: {outcomes:?}"
        );
        assert_eq!(
            outcomes.iter().filter(|outcome| *outcome == "busy").count(),
            1,
            "the competing process must observe a live lease: {outcomes:?}"
        );
    }

    #[test]
    fn refresh_lease_process_helper() {
        let Some(dir) = std::env::var_os("BALANZE_LEASE_TEST_DIR").map(PathBuf::from) else {
            return;
        };
        let child_id = std::env::var("BALANZE_LEASE_TEST_CHILD").unwrap();
        std::fs::write(dir.join(format!("ready-{child_id}")), b"ready").unwrap();
        wait_for_test_file(&dir.join("start"));

        let outcome = match try_acquire_refresh_lease(&dir).unwrap() {
            LeaseAttempt::Acquired(_lease) => {
                std::fs::write(dir.join(format!("result-{child_id}")), b"acquired").unwrap();
                std::thread::sleep(StdDuration::from_millis(500));
                return;
            }
            LeaseAttempt::Busy => "busy",
        };
        std::fs::write(dir.join(format!("result-{child_id}")), outcome).unwrap();
    }

    fn wait_for_test_file(path: &Path) {
        let deadline = std::time::Instant::now() + StdDuration::from_secs(5);
        while !path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            std::thread::sleep(StdDuration::from_millis(5));
        }
    }
}
