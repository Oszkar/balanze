use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike as _, TimeZone as _, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::client::{USER_AGENT, fetch_costs_this_month};
use crate::types::{
    CachedOpenAiCosts, CostsGateError, GateDeferredReason, OpenAiCosts, OpenAiError,
    StoredFailureKind,
};

pub const COSTS_GATE_SECS: i64 = 300;
pub const MAX_GATE_IDENTITIES: usize = 8;

const STORE_SCHEMA_VERSION: u32 = 2;
const OPERATION_TAG: &str = "organization-costs-this-month-v1";
const FILE_NAME: &str = "openai-cost.json";
const LEASE_FILE_PREFIX: &str = "openai-cost.refresh.";
const LEASE_FILE_SUFFIX: &str = ".lease";
const LEGACY_LEASE_FILE_NAME: &str = "openai-cost.refresh.lease";
const LEASE_STALE_AFTER: StdDuration = StdDuration::from_secs(10);
const CONTENTION_WAIT: StdDuration = StdDuration::from_millis(250);
const CONTENTION_POLL: StdDuration = StdDuration::from_millis(20);
static TOKEN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AttemptRecord {
    token: String,
    started_at: DateTime<Utc>,
    state: AttemptState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state", content = "failure")]
enum AttemptState {
    InFlight,
    Success,
    Failure(StoredFailureKind),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
enum StoredSuccess {
    Full(OpenAiCosts),
    LegacyHeadline {
        total_micro_usd: i64,
        fetched_at: DateTime<Utc>,
    },
}

impl StoredSuccess {
    fn total_micro_usd(&self) -> i64 {
        match self {
            Self::Full(costs) => costs.total_micro_usd,
            Self::LegacyHeadline {
                total_micro_usd, ..
            } => *total_micro_usd,
        }
    }

    fn fetched_at(&self) -> DateTime<Utc> {
        match self {
            Self::Full(costs) => costs.fetched_at,
            Self::LegacyHeadline { fetched_at, .. } => *fetched_at,
        }
    }

    fn cached(&self) -> CachedOpenAiCosts {
        match self {
            Self::Full(costs) => CachedOpenAiCosts::Full {
                costs: costs.clone(),
            },
            Self::LegacyHeadline {
                total_micro_usd,
                fetched_at,
            } => CachedOpenAiCosts::LegacyHeadline {
                total_micro_usd: *total_micro_usd,
                fetched_at: *fetched_at,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GateEntry {
    key_fingerprint: String,
    last_attempt: AttemptRecord,
    last_success: Option<StoredSuccess>,
    touched_at: DateTime<Utc>,
}

impl GateEntry {
    fn cached(&self) -> Option<CachedOpenAiCosts> {
        self.last_success.as_ref().map(StoredSuccess::cached)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GateStore {
    schema_version: u32,
    #[serde(default)]
    entries: BTreeMap<String, GateEntry>,
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    total_micro_usd: Option<i64>,
    #[serde(default)]
    fetched_at: Option<DateTime<Utc>>,
    #[serde(default)]
    last_failure_at: Option<DateTime<Utc>>,
}

impl Default for GateStore {
    fn default() -> Self {
        Self {
            schema_version: STORE_SCHEMA_VERSION,
            entries: BTreeMap::new(),
            fingerprint: String::new(),
            total_micro_usd: None,
            fetched_at: None,
            last_failure_at: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct LegacyEntry {
    fingerprint: String,
    total_micro_usd: Option<i64>,
    fetched_at: Option<DateTime<Utc>>,
    last_failure_at: Option<DateTime<Utc>>,
}

enum LeaseAttempt {
    Acquired(StoreLease),
    Busy,
}

struct StoreLease {
    path: PathBuf,
    token: String,
}

impl Drop for StoreLease {
    fn drop(&mut self) {
        match std::fs::read_to_string(&self.path) {
            Ok(current) if current == owner_marker(&self.token) => {
                if let Err(error) = std::fs::remove_file(&self.path)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::debug!("OpenAI Costs gate lease cleanup failed: {error}");
                }
            }
            Ok(_) | Err(_) => {}
        }
    }
}

enum ReserveAttempt {
    Decision(ReserveDecision),
    Busy(Option<CachedOpenAiCosts>),
}

enum ReserveDecision {
    Reserved {
        token: String,
        cached: Option<CachedOpenAiCosts>,
    },
    Current(OpenAiCosts),
    Deferred(CostsGateError),
}

/// Fetch month-to-date Costs through the shared provider gate.
pub async fn gated_costs_this_month(
    base_url: &str,
    admin_key: &str,
    timeout: StdDuration,
) -> Result<OpenAiCosts, CostsGateError> {
    let Some(cache_dir) = cache_dir_path() else {
        return Err(CostsGateError::Unavailable {
            message: "the per-user cache directory could not be resolved".to_string(),
            cached: None,
        });
    };
    gated_costs_this_month_with_cache(base_url, admin_key, timeout, cache_dir).await
}

/// Explicit-cache-root form used by integration tests and subprocess adapters.
#[doc(hidden)]
pub async fn gated_costs_this_month_with_cache(
    base_url: &str,
    admin_key: &str,
    timeout: StdDuration,
    cache_dir: PathBuf,
) -> Result<OpenAiCosts, CostsGateError> {
    costs_this_month_at(base_url, admin_key, timeout, cache_dir, Utc::now()).await
}

async fn costs_this_month_at(
    base_url: &str,
    admin_key: &str,
    timeout: StdDuration,
    cache_dir: PathBuf,
    now: DateTime<Utc>,
) -> Result<OpenAiCosts, CostsGateError> {
    let normalized_base = base_url.trim_end_matches('/').to_string();
    let identity = request_fingerprint(admin_key, &normalized_base);
    let key_fingerprint = key_fingerprint(admin_key);
    let decision = reserve_with_wait(
        cache_dir.clone(),
        identity.clone(),
        key_fingerprint.clone(),
        now,
    )
    .await?;

    let (token, cached) = match decision {
        ReserveDecision::Current(costs) => return Ok(costs),
        ReserveDecision::Deferred(error) => return Err(error),
        ReserveDecision::Reserved { token, cached } => (token, cached),
    };

    let result = match Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => fetch_costs_this_month(&client, &normalized_base, admin_key, now).await,
        Err(error) => Err(OpenAiError::Network(error)),
    };

    match result {
        Ok(costs) => {
            complete_best_effort(
                cache_dir,
                identity,
                key_fingerprint,
                token,
                now,
                Ok(costs.clone()),
            )
            .await;
            Ok(costs)
        }
        Err(source) => {
            let failure = StoredFailureKind::from(&source);
            complete_best_effort(
                cache_dir,
                identity,
                key_fingerprint,
                token,
                now,
                Err(failure),
            )
            .await;
            Err(CostsGateError::Provider { source, cached })
        }
    }
}

async fn reserve_with_wait(
    cache_dir: PathBuf,
    identity: String,
    key_fingerprint: String,
    now: DateTime<Utc>,
) -> Result<ReserveDecision, CostsGateError> {
    let deadline = tokio::time::Instant::now() + CONTENTION_WAIT;
    loop {
        let dir = cache_dir.clone();
        let request_id = identity.clone();
        let key_id = key_fingerprint.clone();
        let attempt =
            tokio::task::spawn_blocking(move || reserve_once(&dir, &request_id, &key_id, now))
                .await
                .map_err(|_| CostsGateError::Unavailable {
                    message: "the gate file worker stopped unexpectedly".to_string(),
                    cached: None,
                })?
                .map_err(|error| CostsGateError::Unavailable {
                    message: error.to_string(),
                    cached: read_cached_best_effort(&cache_dir, &identity, &key_fingerprint, now),
                })?;

        match attempt {
            ReserveAttempt::Decision(decision) => return Ok(decision),
            ReserveAttempt::Busy(cached) if cached.is_some() => {
                return Ok(ReserveDecision::Deferred(CostsGateError::Deferred {
                    reason: GateDeferredReason::LeaseBusy,
                    retry_after_secs: 1,
                    failure: None,
                    cached,
                }));
            }
            ReserveAttempt::Busy(cached) if tokio::time::Instant::now() >= deadline => {
                return Ok(ReserveDecision::Deferred(CostsGateError::Deferred {
                    reason: GateDeferredReason::LeaseBusy,
                    retry_after_secs: 1,
                    failure: None,
                    cached,
                }));
            }
            ReserveAttempt::Busy(_) => tokio::time::sleep(CONTENTION_POLL).await,
        }
    }
}

fn reserve_once(
    cache_dir: &Path,
    identity: &str,
    key_fingerprint: &str,
    now: DateTime<Utc>,
) -> std::io::Result<ReserveAttempt> {
    let lease = match try_acquire_store_lease(cache_dir)? {
        LeaseAttempt::Acquired(lease) => lease,
        LeaseAttempt::Busy => {
            return Ok(ReserveAttempt::Busy(read_cached_best_effort(
                cache_dir,
                identity,
                key_fingerprint,
                now,
            )));
        }
    };

    let mut store = load_store(cache_dir, identity, key_fingerprint, now)?;
    let mut normalized = false;
    if let Some(entry) = store.entries.get_mut(identity)
        && entry.last_attempt.started_at > now
    {
        entry.last_attempt.started_at = now;
        entry.touched_at = now;
        normalized = true;
    }

    if let Some(entry) = store.entries.get(identity)
        && let Some(remaining) = gate_remaining(entry.last_attempt.started_at, now)
    {
        let decision = active_decision(entry, now, remaining);
        if normalized {
            write_store(cache_dir, &mut store)?;
            sweep_stale_leases_best_effort(cache_dir, &lease.path, SystemTime::now());
        }
        drop(lease);
        return Ok(ReserveAttempt::Decision(decision));
    }

    if !store.entries.contains_key(identity) {
        prune_expired_entries(&mut store, now);
        if store.entries.len() >= MAX_GATE_IDENTITIES {
            let cached = store.entries.get(identity).and_then(GateEntry::cached);
            drop(lease);
            return Ok(ReserveAttempt::Decision(ReserveDecision::Deferred(
                CostsGateError::Deferred {
                    reason: GateDeferredReason::Capacity,
                    retry_after_secs: COSTS_GATE_SECS as u64,
                    failure: None,
                    cached,
                },
            )));
        }
    }

    let token = unique_token();
    let prior = store
        .entries
        .get(identity)
        .and_then(|entry| entry.last_success.clone());
    let cached = prior.as_ref().map(StoredSuccess::cached);
    store.entries.insert(
        identity.to_string(),
        GateEntry {
            key_fingerprint: key_fingerprint.to_string(),
            last_attempt: AttemptRecord {
                token: token.clone(),
                started_at: now,
                state: AttemptState::InFlight,
            },
            last_success: prior,
            touched_at: now,
        },
    );
    write_store(cache_dir, &mut store)?;
    sweep_stale_leases_best_effort(cache_dir, &lease.path, SystemTime::now());
    drop(lease);
    Ok(ReserveAttempt::Decision(ReserveDecision::Reserved {
        token,
        cached,
    }))
}

fn active_decision(entry: &GateEntry, now: DateTime<Utc>, remaining: u64) -> ReserveDecision {
    let cached = entry.cached();
    match (&entry.last_attempt.state, entry.last_success.as_ref()) {
        (AttemptState::Success, Some(StoredSuccess::Full(costs)))
            if costs.start_time == first_of_month(now) =>
        {
            ReserveDecision::Current(costs.clone())
        }
        (AttemptState::InFlight, _) => ReserveDecision::Deferred(CostsGateError::Deferred {
            reason: GateDeferredReason::InFlight,
            retry_after_secs: remaining,
            failure: None,
            cached,
        }),
        (AttemptState::Failure(failure), _) => {
            ReserveDecision::Deferred(CostsGateError::Deferred {
                reason: GateDeferredReason::RecentAttempt,
                retry_after_secs: remaining,
                failure: Some(*failure),
                cached,
            })
        }
        (AttemptState::Success, Some(StoredSuccess::LegacyHeadline { .. })) => {
            ReserveDecision::Deferred(CostsGateError::Deferred {
                reason: GateDeferredReason::LegacyHeadline,
                retry_after_secs: remaining,
                failure: None,
                cached,
            })
        }
        (AttemptState::Success, Some(StoredSuccess::Full(_))) => {
            ReserveDecision::Deferred(CostsGateError::Deferred {
                reason: GateDeferredReason::PriorMonth,
                retry_after_secs: remaining,
                failure: None,
                cached,
            })
        }
        (AttemptState::Success, None) => ReserveDecision::Deferred(CostsGateError::Deferred {
            reason: GateDeferredReason::RecentAttempt,
            retry_after_secs: remaining,
            failure: None,
            cached: None,
        }),
    }
}

async fn complete_best_effort(
    cache_dir: PathBuf,
    identity: String,
    key_fingerprint: String,
    token: String,
    now: DateTime<Utc>,
    result: Result<OpenAiCosts, StoredFailureKind>,
) {
    let deadline = tokio::time::Instant::now() + CONTENTION_WAIT;
    loop {
        let dir = cache_dir.clone();
        let request_id = identity.clone();
        let key_id = key_fingerprint.clone();
        let attempt_token = token.clone();
        let completion = result.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            complete_once(&dir, &request_id, &key_id, &attempt_token, now, completion)
        })
        .await;

        match outcome {
            Ok(Ok(true)) => return,
            Ok(Ok(false)) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(CONTENTION_POLL).await;
            }
            Ok(Ok(false)) => return,
            Ok(Err(error)) => {
                tracing::debug!("OpenAI Costs gate completion failed: {error}");
                return;
            }
            Err(error) => {
                tracing::debug!("OpenAI Costs gate completion worker failed: {error}");
                return;
            }
        }
    }
}

fn complete_once(
    cache_dir: &Path,
    identity: &str,
    key_fingerprint: &str,
    token: &str,
    now: DateTime<Utc>,
    result: Result<OpenAiCosts, StoredFailureKind>,
) -> std::io::Result<bool> {
    let lease = match try_acquire_store_lease(cache_dir)? {
        LeaseAttempt::Acquired(lease) => lease,
        LeaseAttempt::Busy => return Ok(false),
    };
    let mut store = load_store(cache_dir, identity, key_fingerprint, now)?;
    let Some(entry) = store.entries.get_mut(identity) else {
        return Ok(true);
    };
    if entry.last_attempt.token != token {
        return Ok(true);
    }
    match result {
        Ok(costs) => {
            entry.last_attempt.state = AttemptState::Success;
            entry.last_success = Some(StoredSuccess::Full(costs));
        }
        Err(failure) => entry.last_attempt.state = AttemptState::Failure(failure),
    }
    entry.touched_at = now;
    write_store(cache_dir, &mut store)?;
    sweep_stale_leases_best_effort(cache_dir, &lease.path, SystemTime::now());
    drop(lease);
    Ok(true)
}

fn load_store(
    cache_dir: &Path,
    identity: &str,
    key_fingerprint: &str,
    now: DateTime<Utc>,
) -> std::io::Result<GateStore> {
    let path = cache_dir.join(FILE_NAME);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(GateStore::default());
        }
        Err(error) => return Err(error),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(invalid_data)?;
    if let Some(version) = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        if version != u64::from(STORE_SCHEMA_VERSION) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unsupported OpenAI Costs gate schema version {version}"),
            ));
        }
        return serde_json::from_value(value).map_err(invalid_data);
    }

    let legacy: LegacyEntry = serde_json::from_value(value).map_err(invalid_data)?;
    let mut store = GateStore::default();
    if legacy.fingerprint != key_fingerprint {
        return Ok(store);
    }
    let started_at = legacy.last_failure_at.or(legacy.fetched_at).unwrap_or(now);
    let state = if legacy.last_failure_at.is_some() {
        AttemptState::Failure(StoredFailureKind::Network)
    } else {
        AttemptState::Success
    };
    let last_success =
        legacy
            .total_micro_usd
            .zip(legacy.fetched_at)
            .map(
                |(total_micro_usd, fetched_at)| StoredSuccess::LegacyHeadline {
                    total_micro_usd,
                    fetched_at,
                },
            );
    store.entries.insert(
        identity.to_string(),
        GateEntry {
            key_fingerprint: key_fingerprint.to_string(),
            last_attempt: AttemptRecord {
                token: format!("legacy-{}", started_at.timestamp_millis()),
                started_at,
                state,
            },
            last_success,
            touched_at: started_at,
        },
    );
    Ok(store)
}

fn read_cached_best_effort(
    cache_dir: &Path,
    identity: &str,
    key_fingerprint: &str,
    now: DateTime<Utc>,
) -> Option<CachedOpenAiCosts> {
    load_store(cache_dir, identity, key_fingerprint, now)
        .ok()?
        .entries
        .get(identity)
        .and_then(GateEntry::cached)
}

fn write_store(cache_dir: &Path, store: &mut GateStore) -> std::io::Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    refresh_legacy_projection(store);
    let bytes = serde_json::to_vec(store).map_err(invalid_data)?;
    atomic_file::atomic_write(
        &cache_dir.join(FILE_NAME),
        &bytes,
        atomic_file::Permissions::Default,
    )
}

fn refresh_legacy_projection(store: &mut GateStore) {
    let Some(entry) = store.entries.values().max_by_key(|entry| entry.touched_at) else {
        store.fingerprint.clear();
        store.total_micro_usd = None;
        store.fetched_at = None;
        store.last_failure_at = None;
        return;
    };
    store.fingerprint.clone_from(&entry.key_fingerprint);
    store.total_micro_usd = entry
        .last_success
        .as_ref()
        .map(StoredSuccess::total_micro_usd);
    store.fetched_at = entry.last_success.as_ref().map(StoredSuccess::fetched_at);
    store.last_failure_at = match entry.last_attempt.state {
        AttemptState::InFlight | AttemptState::Failure(_) => Some(entry.last_attempt.started_at),
        AttemptState::Success => None,
    };
}

fn prune_expired_entries(store: &mut GateStore, now: DateTime<Utc>) {
    if store.entries.len() < MAX_GATE_IDENTITIES {
        return;
    }
    let mut expired: Vec<_> = store
        .entries
        .iter()
        .filter(|(_, entry)| gate_remaining(entry.last_attempt.started_at, now).is_none())
        .map(|(identity, entry)| (identity.clone(), entry.last_attempt.started_at))
        .collect();
    expired.sort_by_key(|(_, started_at)| *started_at);
    for (identity, _) in expired {
        if store.entries.len() < MAX_GATE_IDENTITIES {
            break;
        }
        store.entries.remove(&identity);
    }
}

fn gate_remaining(started_at: DateTime<Utc>, now: DateTime<Utc>) -> Option<u64> {
    let age = now.signed_duration_since(started_at).num_seconds();
    (age < COSTS_GATE_SECS).then(|| (COSTS_GATE_SECS - age.max(0)) as u64)
}

fn first_of_month(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .expect("constructing first-of-month always succeeds for a Utc now")
}

pub fn cache_dir_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("BALANZE_CACHE_DIR_OVERRIDE") {
        return Some(PathBuf::from(dir).join("statusline"));
    }
    directories::ProjectDirs::from("me", "oszkar", "Balanze")
        .map(|dirs| dirs.cache_dir().join("statusline"))
}

fn request_fingerprint(admin_key: &str, base_url: &str) -> String {
    fnv1a([
        admin_key.as_bytes(),
        &[0],
        base_url.trim_end_matches('/').as_bytes(),
        &[0],
        OPERATION_TAG.as_bytes(),
    ])
}

fn key_fingerprint(admin_key: &str) -> String {
    fnv1a([admin_key.as_bytes()])
}

fn fnv1a<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

fn try_acquire_store_lease(cache_dir: &Path) -> std::io::Result<LeaseAttempt> {
    std::fs::create_dir_all(cache_dir)?;
    for _ in 0..4 {
        let token = unique_token();
        let path = cache_dir.join(format!("{LEASE_FILE_PREFIX}{token}{LEASE_FILE_SUFFIX}"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let candidate = candidate_marker(&token);
                if let Err(error) = file
                    .write_all(candidate.as_bytes())
                    .and_then(|()| file.sync_all())
                {
                    drop(file);
                    let _ = std::fs::remove_file(&path);
                    return Err(error);
                }
                if has_live_owner_or_preceding_candidate(
                    cache_dir,
                    &path,
                    &token,
                    SystemTime::now(),
                )? {
                    drop(file);
                    remove_candidate(&path, &candidate);
                    return Ok(LeaseAttempt::Busy);
                }

                let owner = owner_marker(&token);
                if let Err(error) = file
                    .set_len(0)
                    .and_then(|()| file.seek(std::io::SeekFrom::Start(0)).map(|_| ()))
                    .and_then(|()| file.write_all(owner.as_bytes()))
                    .and_then(|()| file.sync_all())
                {
                    drop(file);
                    let _ = std::fs::remove_file(&path);
                    return Err(error);
                }
                let lease = StoreLease { path, token };
                return Ok(LeaseAttempt::Acquired(lease));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(LeaseAttempt::Busy)
}

fn has_live_owner_or_preceding_candidate(
    cache_dir: &Path,
    own_path: &Path,
    own_token: &str,
    now: SystemTime,
) -> std::io::Result<bool> {
    for entry in std::fs::read_dir(cache_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path == own_path || !is_lease_candidate(&entry.file_name()) {
            continue;
        }
        if lease_is_stale(&path, now) {
            continue;
        }
        let name = entry.file_name();
        if name == LEGACY_LEASE_FILE_NAME {
            return Ok(true);
        }
        let marker = match std::fs::read_to_string(&path) {
            Ok(marker) => marker,
            Err(_) => return Ok(true),
        };
        match marker.strip_prefix("candidate:") {
            Some(other_token) if other_token < own_token => return Ok(true),
            Some(_) => {}
            None => return Ok(true),
        }
    }
    Ok(false)
}

fn candidate_marker(token: &str) -> String {
    format!("candidate:{token}")
}

fn owner_marker(token: &str) -> String {
    format!("owner:{token}")
}

fn remove_candidate(path: &Path, expected: &str) {
    if std::fs::read_to_string(path).is_ok_and(|current| current == expected) {
        let _ = std::fs::remove_file(path);
    }
}

fn is_lease_candidate(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name == LEGACY_LEASE_FILE_NAME
        || (name.starts_with(LEASE_FILE_PREFIX) && name.ends_with(LEASE_FILE_SUFFIX))
}

fn lease_is_stale(path: &Path, now: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .is_some_and(|modified| {
            now.duration_since(modified)
                .map_or(true, |age| age >= LEASE_STALE_AFTER)
        })
}

/// Remove abandoned candidates only after this owner has durably published.
/// Acquisition itself remains delete-free, so concurrent contenders never
/// race by unlinking a file another process is still promoting.
fn sweep_stale_leases_best_effort(cache_dir: &Path, own_path: &Path, now: SystemTime) {
    let entries = match std::fs::read_dir(cache_dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::debug!("OpenAI Costs gate lease sweep failed: {error}");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path != own_path
            && is_lease_candidate(&entry.file_name())
            && lease_is_stale(&path, now)
            && let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!("OpenAI Costs gate stale lease cleanup failed: {error}");
        }
    }
}

fn unique_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = TOKEN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    // Fixed-width tokens give simultaneous candidate files a deterministic
    // order. Once a candidate promotes itself to owner, all later contenders
    // yield regardless of token ordering.
    format!("{nanos:039}-{:010}-{sequence:020}", std::process::id())
}

fn invalid_data(error: impl std::error::Error + Send + Sync + 'static) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use tempfile::tempdir;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 14, 12, 0, 0).unwrap()
    }

    fn costs(now: DateTime<Utc>, total_micro_usd: i64) -> OpenAiCosts {
        OpenAiCosts {
            start_time: first_of_month(now),
            end_time: now,
            total_micro_usd,
            by_line_item: Vec::new(),
            truncated: false,
            fetched_at: now,
        }
    }

    #[test]
    fn request_fingerprint_is_stable_and_scoped_to_key_base_and_operation() {
        let a = request_fingerprint("key-a", "https://api.openai.com/");
        assert_eq!(a, request_fingerprint("key-a", "https://api.openai.com"));
        assert_ne!(a, request_fingerprint("key-b", "https://api.openai.com"));
        assert_ne!(a, request_fingerprint("key-a", "https://example.test"));
        assert!(!a.contains("key-a"));
        assert!(!a.contains("api.openai.com"));
    }

    #[test]
    fn full_and_legacy_successes_have_one_authoritative_headline() {
        let full = StoredSuccess::Full(costs(t0(), 4_200_000));
        let legacy = StoredSuccess::LegacyHeadline {
            total_micro_usd: 3_100_000,
            fetched_at: t0(),
        };
        assert_eq!(full.total_micro_usd(), 4_200_000);
        assert_eq!(legacy.total_micro_usd(), 3_100_000);
        assert!(matches!(full, StoredSuccess::Full(_)));
        assert!(matches!(legacy, StoredSuccess::LegacyHeadline { .. }));
    }

    #[test]
    fn reservation_is_active_until_exactly_300_seconds() {
        assert_eq!(gate_remaining(t0(), t0() + Duration::seconds(299)), Some(1));
        assert_eq!(gate_remaining(t0(), t0() + Duration::seconds(300)), None);
    }

    #[test]
    fn month_rollover_does_not_bypass_an_active_attempt() {
        let start = Utc.with_ymd_and_hms(2026, 8, 31, 23, 59, 59).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        assert_eq!(gate_remaining(start, now), Some(299));
    }

    #[test]
    fn legacy_entry_migrates_to_headline_only_variant() {
        let dir = tempdir().unwrap();
        let key = "key-a";
        let legacy = serde_json::json!({
            "fingerprint": key_fingerprint(key),
            "total_micro_usd": 123,
            "fetched_at": t0(),
            "last_failure_at": null
        });
        std::fs::write(
            dir.path().join(FILE_NAME),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        let identity = request_fingerprint(key, "https://api.openai.com");
        let store = load_store(dir.path(), &identity, &key_fingerprint(key), t0()).unwrap();
        assert!(matches!(
            store.entries[&identity].last_success,
            Some(StoredSuccess::LegacyHeadline { .. })
        ));
    }

    #[test]
    fn reservation_failure_happens_before_any_network_callback() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("not-a-directory");
        std::fs::write(&file, b"x").unwrap();
        let identity = request_fingerprint("key-a", "https://api.openai.com");
        assert!(reserve_once(&file, &identity, &key_fingerprint("key-a"), t0()).is_err());
    }

    #[test]
    fn late_completion_cannot_overwrite_a_successor() {
        let dir = tempdir().unwrap();
        let identity = request_fingerprint("key-a", "https://api.openai.com");
        let key_id = key_fingerprint("key-a");
        let ReserveAttempt::Decision(ReserveDecision::Reserved { token, .. }) =
            reserve_once(dir.path(), &identity, &key_id, t0()).unwrap()
        else {
            panic!("first reservation expected");
        };
        let successor_time = t0() + Duration::seconds(300);
        let ReserveAttempt::Decision(ReserveDecision::Reserved {
            token: successor, ..
        }) = reserve_once(dir.path(), &identity, &key_id, successor_time).unwrap()
        else {
            panic!("successor reservation expected");
        };
        assert_ne!(token, successor);
        complete_once(
            dir.path(),
            &identity,
            &key_id,
            &token,
            successor_time,
            Ok(costs(t0(), 1)),
        )
        .unwrap();
        let store = load_store(dir.path(), &identity, &key_id, successor_time).unwrap();
        assert_eq!(store.entries[&identity].last_attempt.token, successor);
        assert!(matches!(
            store.entries[&identity].last_attempt.state,
            AttemptState::InFlight
        ));
    }

    #[test]
    fn active_entries_are_not_pruned() {
        let mut store = GateStore::default();
        for index in 0..MAX_GATE_IDENTITIES {
            store.entries.insert(
                format!("id-{index}"),
                GateEntry {
                    key_fingerprint: format!("key-{index}"),
                    last_attempt: AttemptRecord {
                        token: format!("token-{index}"),
                        started_at: t0(),
                        state: AttemptState::InFlight,
                    },
                    last_success: None,
                    touched_at: t0(),
                },
            );
        }
        prune_expired_entries(&mut store, t0() + Duration::seconds(299));
        assert_eq!(store.entries.len(), MAX_GATE_IDENTITIES);
    }

    #[test]
    fn expired_entries_prune_oldest_first() {
        let mut store = GateStore::default();
        for index in 0..=MAX_GATE_IDENTITIES {
            let started_at = t0() - Duration::seconds((index + 1) as i64);
            store.entries.insert(
                format!("id-{index}"),
                GateEntry {
                    key_fingerprint: format!("key-{index}"),
                    last_attempt: AttemptRecord {
                        token: format!("token-{index}"),
                        started_at,
                        state: AttemptState::Success,
                    },
                    last_success: Some(StoredSuccess::Full(costs(started_at, index as i64))),
                    touched_at: started_at,
                },
            );
        }
        prune_expired_entries(&mut store, t0() + Duration::seconds(600));
        assert_eq!(store.entries.len(), MAX_GATE_IDENTITIES - 1);
        assert!(
            !store
                .entries
                .contains_key(&format!("id-{MAX_GATE_IDENTITIES}"))
        );
    }

    #[test]
    fn serialized_store_contains_no_raw_key_or_base_url() {
        let dir = tempdir().unwrap();
        let key = "private-admin-key-material";
        let base = "https://private-openai-proxy.example";
        let identity = request_fingerprint(key, base);
        let key_id = key_fingerprint(key);
        reserve_once(dir.path(), &identity, &key_id, t0()).unwrap();
        let document = std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        assert!(!document.contains(key));
        assert!(!document.contains(base));
    }

    #[test]
    fn multi_entry_round_trip_preserves_full_data_and_safe_failure_state() {
        let dir = tempdir().unwrap();
        let first_identity = request_fingerprint("key-a", "https://api.openai.com");
        let first_key = key_fingerprint("key-a");
        let ReserveAttempt::Decision(ReserveDecision::Reserved {
            token: first_token, ..
        }) = reserve_once(dir.path(), &first_identity, &first_key, t0()).unwrap()
        else {
            panic!("first identity must reserve");
        };
        let mut full = costs(t0(), 4_200_000);
        full.by_line_item.push(crate::LineItemCost {
            line_item: "gpt-5".to_string(),
            amount_micro_usd: 4_200_000,
        });
        complete_once(
            dir.path(),
            &first_identity,
            &first_key,
            &first_token,
            t0(),
            Ok(full.clone()),
        )
        .unwrap();

        let second_identity = request_fingerprint("key-b", "https://api.openai.com");
        let second_key = key_fingerprint("key-b");
        let ReserveAttempt::Decision(ReserveDecision::Reserved {
            token: second_token,
            ..
        }) = reserve_once(
            dir.path(),
            &second_identity,
            &second_key,
            t0() + Duration::seconds(1),
        )
        .unwrap()
        else {
            panic!("second identity must reserve");
        };
        complete_once(
            dir.path(),
            &second_identity,
            &second_key,
            &second_token,
            t0() + Duration::seconds(1),
            Err(StoredFailureKind::RateLimited),
        )
        .unwrap();

        let store = load_store(dir.path(), &first_identity, &first_key, t0()).unwrap();
        assert_eq!(store.entries.len(), 2);
        assert_eq!(
            store.entries[&first_identity].last_success,
            Some(StoredSuccess::Full(full))
        );
        assert!(matches!(
            store.entries[&second_identity].last_attempt.state,
            AttemptState::Failure(StoredFailureKind::RateLimited)
        ));
    }

    #[test]
    fn legacy_projection_uses_the_full_entry_derived_headline() {
        let dir = tempdir().unwrap();
        let identity = request_fingerprint("key-a", "https://api.openai.com");
        let key_id = key_fingerprint("key-a");
        let ReserveAttempt::Decision(ReserveDecision::Reserved { token, .. }) =
            reserve_once(dir.path(), &identity, &key_id, t0()).unwrap()
        else {
            panic!("identity must reserve");
        };
        complete_once(
            dir.path(),
            &identity,
            &key_id,
            &token,
            t0(),
            Ok(costs(t0(), 9_900_000)),
        )
        .unwrap();

        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(document["fingerprint"], key_id);
        assert_eq!(document["total_micro_usd"], 9_900_000);
        assert_eq!(document["fetched_at"], serde_json::to_value(t0()).unwrap());
        assert!(document["last_failure_at"].is_null());
        let stored_entry = &document["entries"][&identity];
        assert!(stored_entry.get("total_micro_usd").is_none());
        assert_eq!(
            stored_entry["last_success"]["value"]["total_micro_usd"],
            9_900_000
        );
    }

    #[test]
    fn future_attempt_is_normalized_once_then_keeps_a_full_gate() {
        let dir = tempdir().unwrap();
        let identity = request_fingerprint("key-a", "https://api.openai.com");
        let key_id = key_fingerprint("key-a");
        reserve_once(
            dir.path(),
            &identity,
            &key_id,
            t0() + Duration::seconds(600),
        )
        .unwrap();

        assert!(matches!(
            reserve_once(dir.path(), &identity, &key_id, t0()).unwrap(),
            ReserveAttempt::Decision(ReserveDecision::Deferred(_))
        ));
        let normalized = load_store(dir.path(), &identity, &key_id, t0()).unwrap();
        assert_eq!(normalized.entries[&identity].last_attempt.started_at, t0());
        assert!(matches!(
            reserve_once(
                dir.path(),
                &identity,
                &key_id,
                t0() + Duration::seconds(300)
            )
            .unwrap(),
            ReserveAttempt::Decision(ReserveDecision::Reserved { .. })
        ));
    }

    #[test]
    fn ninth_identity_fails_closed_while_eight_are_active() {
        let dir = tempdir().unwrap();
        for index in 0..MAX_GATE_IDENTITIES {
            let key = format!("key-{index}");
            let identity = request_fingerprint(&key, "https://api.openai.com");
            assert!(matches!(
                reserve_once(dir.path(), &identity, &key_fingerprint(&key), t0()).unwrap(),
                ReserveAttempt::Decision(ReserveDecision::Reserved { .. })
            ));
        }
        let ninth = reserve_once(
            dir.path(),
            &request_fingerprint("key-nine", "https://api.openai.com"),
            &key_fingerprint("key-nine"),
            t0(),
        )
        .unwrap();
        assert!(matches!(
            ninth,
            ReserveAttempt::Decision(ReserveDecision::Deferred(CostsGateError::Deferred {
                reason: GateDeferredReason::Capacity,
                ..
            }))
        ));
    }

    #[test]
    fn live_store_lease_excludes_a_second_owner() {
        let dir = tempdir().unwrap();
        let first = match try_acquire_store_lease(dir.path()).unwrap() {
            LeaseAttempt::Acquired(lease) => lease,
            LeaseAttempt::Busy => panic!("first lease must be acquired"),
        };
        assert!(matches!(
            try_acquire_store_lease(dir.path()).unwrap(),
            LeaseAttempt::Busy
        ));
        drop(first);
        assert!(matches!(
            try_acquire_store_lease(dir.path()).unwrap(),
            LeaseAttempt::Acquired(_)
        ));
    }

    #[test]
    fn live_legacy_lease_blocks_the_new_store_protocol() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(LEGACY_LEASE_FILE_NAME), b"legacy").unwrap();
        assert!(matches!(
            try_acquire_store_lease(dir.path()).unwrap(),
            LeaseAttempt::Busy
        ));
    }

    #[test]
    fn abandoned_lease_recovers_and_old_owner_cannot_remove_successor() {
        let dir = tempdir().unwrap();
        let old = match try_acquire_store_lease(dir.path()).unwrap() {
            LeaseAttempt::Acquired(lease) => lease,
            LeaseAttempt::Busy => panic!("first lease must be acquired"),
        };
        let old_path = old.path.clone();
        let file = OpenOptions::new().write(true).open(&old_path).unwrap();
        file.set_modified(SystemTime::now() - LEASE_STALE_AFTER - StdDuration::from_secs(1))
            .unwrap();

        let successor = match try_acquire_store_lease(dir.path()).unwrap() {
            LeaseAttempt::Acquired(lease) => lease,
            LeaseAttempt::Busy => panic!("stale lease must be recoverable"),
        };
        let successor_path = successor.path.clone();
        assert_ne!(old_path, successor_path);
        assert!(old_path.exists());
        drop(old);
        assert!(successor_path.exists());
        drop(successor);
        assert!(!successor_path.exists());
    }

    #[test]
    fn future_dated_lease_does_not_wedge_acquisition() {
        let dir = tempdir().unwrap();
        let future = dir
            .path()
            .join(format!("{LEASE_FILE_PREFIX}future{LEASE_FILE_SUFFIX}"));
        std::fs::write(&future, b"owner:future").unwrap();
        let file = OpenOptions::new().write(true).open(&future).unwrap();
        file.set_modified(SystemTime::now() + StdDuration::from_secs(60))
            .unwrap();

        assert!(matches!(
            try_acquire_store_lease(dir.path()).unwrap(),
            LeaseAttempt::Acquired(_)
        ));
    }

    #[test]
    fn successful_publish_sweeps_abandoned_lease_files() {
        let dir = tempdir().unwrap();
        let abandoned = dir
            .path()
            .join(format!("{LEASE_FILE_PREFIX}abandoned{LEASE_FILE_SUFFIX}"));
        std::fs::write(&abandoned, b"owner:abandoned").unwrap();
        let file = OpenOptions::new().write(true).open(&abandoned).unwrap();
        file.set_modified(SystemTime::now() - LEASE_STALE_AFTER - StdDuration::from_secs(1))
            .unwrap();

        let decision = reserve_once(
            dir.path(),
            &request_fingerprint("key", "https://api.openai.com"),
            &key_fingerprint("key"),
            t0(),
        )
        .unwrap();

        assert!(matches!(decision, ReserveAttempt::Decision(_)));
        assert!(!abandoned.exists());
    }

    #[test]
    fn stale_lease_recovery_is_exclusive_across_processes() {
        let dir = tempdir().unwrap();
        let abandoned = dir
            .path()
            .join(format!("{LEASE_FILE_PREFIX}abandoned{LEASE_FILE_SUFFIX}"));
        std::fs::write(&abandoned, b"abandoned").unwrap();
        let file = OpenOptions::new().write(true).open(&abandoned).unwrap();
        file.set_modified(SystemTime::now() - LEASE_STALE_AFTER - StdDuration::from_secs(1))
            .unwrap();

        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for child_id in 0..2 {
            children.push(
                std::process::Command::new(&executable)
                    .args([
                        "--exact",
                        "gate::tests::store_lease_process_helper",
                        "--nocapture",
                    ])
                    .env("BALANZE_GATE_TEST_DIR", dir.path())
                    .env("BALANZE_GATE_TEST_CHILD", child_id.to_string())
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
        assert!(abandoned.exists());

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
            "stale recovery must elect exactly one owner: {outcomes:?}"
        );
        assert!(matches!(
            try_acquire_store_lease(dir.path()).unwrap(),
            LeaseAttempt::Acquired(_)
        ));
    }

    #[test]
    fn store_lease_process_helper() {
        let Some(dir) = std::env::var_os("BALANZE_GATE_TEST_DIR").map(PathBuf::from) else {
            return;
        };
        let child_id = std::env::var("BALANZE_GATE_TEST_CHILD").unwrap();
        std::fs::write(dir.join(format!("ready-{child_id}")), b"ready").unwrap();
        wait_for_test_file(&dir.join("start"));

        match try_acquire_store_lease(&dir).unwrap() {
            LeaseAttempt::Acquired(_lease) => {
                std::fs::write(dir.join(format!("result-{child_id}")), b"acquired").unwrap();
                std::thread::sleep(StdDuration::from_millis(500));
            }
            LeaseAttempt::Busy => {
                std::fs::write(dir.join(format!("result-{child_id}")), b"busy").unwrap();
            }
        }
    }

    fn wait_for_test_file(path: &Path) {
        let deadline = std::time::Instant::now() + StdDuration::from_secs(5);
        while !path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            std::thread::sleep(StdDuration::from_millis(10));
        }
    }
}
