//! Pure, network-free cache for the self-composed OpenAI cost figure.
//!
//! One global entry per machine (NOT per transcript): OpenAI costs are
//! account-wide, and AGENTS.md 3.1 requires the billing fetch be gated to at
//! most once per 300s machine-wide. The entry is invalidated by a fingerprint
//! of the resolved OpenAI key, so a key rotation forces a refetch and distinct
//! keys never share a value. The fingerprint is a hash, never the key itself
//! (3.4 secret hygiene). The 300s TTL IS the 3.1 politeness gate; the failure
//! cooldown keeps a broken API from being retried every turn.

use std::path::{Path, PathBuf};

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

/// Seed the cache from an OpenAI value fetched elsewhere (the watcher's
/// `snapshot.json`), so the machine-wide 300s gate accounts for that fetch and
/// the statusline does not immediately re-poll at the watcher handoff (AGENTS.md
/// 3.1). No-op if the stored entry was already fetched at or after `fetched_at`
/// (never regress a fresher statusline-side fetch). Any active failure cooldown
/// is preserved so seeding a watcher value cannot bypass the gate.
pub fn seed_if_newer(
    dir: &Path,
    fingerprint: &str,
    total_micro_usd: i64,
    fetched_at: DateTime<Utc>,
) {
    let existing = read(dir, fingerprint);
    if existing
        .as_ref()
        .and_then(|e| e.fetched_at)
        .is_some_and(|t| t >= fetched_at)
    {
        return;
    }
    write(
        dir,
        &OpenAiCostEntry {
            fingerprint: fingerprint.to_string(),
            total_micro_usd: Some(total_micro_usd),
            fetched_at: Some(fetched_at),
            last_failure_at: existing.and_then(|e| e.last_failure_at),
        },
    );
}

/// Best-effort atomic write (tmp + rename). Errors are logged at debug and
/// swallowed - a cache write failure must never break the statusline.
fn write(dir: &Path, entry: &OpenAiCostEntry) {
    if let Err(e) = try_write(dir, entry) {
        tracing::debug!("statusline cache write failed: {e}");
    }
}

fn try_write(dir: &Path, entry: &OpenAiCostEntry) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let final_path = dir.join(FILE_NAME);
    // Fixed tmp name: concurrent writers from distinct processes can silently lose the race (rename is still atomic, so the file is never corrupt). Acceptable for a best-effort cache.
    let tmp_path = dir.join(format!("{FILE_NAME}.tmp"));
    let bytes = serde_json::to_vec(entry).map_err(std::io::Error::other)?;
    std::fs::write(&tmp_path, &bytes)?;
    // No sync_all: best-effort cache; a crash before flush just triggers a refetch.
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
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
    fn seed_if_newer_seeds_when_empty_and_reads_fresh() {
        let dir = tempdir().unwrap();
        seed_if_newer(dir.path(), "fp", 4_200_000, t0());
        let e = read(dir.path(), "fp").expect("entry");
        assert_eq!(e.total_micro_usd, Some(4_200_000));
        assert_eq!(e.fetched_at, Some(t0()));
        assert!(is_fresh(&e, t0() + Duration::seconds(120)));
    }

    #[test]
    fn seed_if_newer_does_not_regress_a_fresher_entry() {
        let dir = tempdir().unwrap();
        // A newer statusline-side fetch is already recorded.
        write_success(dir.path(), "fp", 999, t0() + Duration::seconds(100));
        // Seeding an OLDER watcher value must not overwrite it.
        seed_if_newer(dir.path(), "fp", 1, t0());
        let e = read(dir.path(), "fp").expect("entry");
        assert_eq!(e.total_micro_usd, Some(999));
        assert_eq!(e.fetched_at, Some(t0() + Duration::seconds(100)));
    }

    #[test]
    fn seed_if_newer_preserves_active_failure_cooldown() {
        let dir = tempdir().unwrap();
        // A recent statusline-side failure -> cooldown active.
        write_failure(dir.path(), "fp", t0() + Duration::seconds(250));
        // An earlier watcher value is seeded; the value lands but the cooldown
        // must survive so seeding cannot bypass the gate.
        seed_if_newer(dir.path(), "fp", 500, t0());
        let e = read(dir.path(), "fp").expect("entry");
        assert_eq!(e.total_micro_usd, Some(500), "watcher value seeded");
        assert!(in_cooldown(&e, t0() + Duration::seconds(300)));
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
}
