//! Headless integration probes shared by `doctor` (diagnostics) and `setup`
//! (readiness summary). Each probe reads a single integration and returns a
//! `CheckResult`. The reusable units are the crate-level read APIs
//! (anthropic_oauth, codex_local, claude_statusline, settings, claude_parser,
//! keychain) - setup.rs's interactive wizard logic is NOT reused (it is
//! stdin / eprintln coupled). AGENTS.md §2 (DRY): one probe definition, two
//! callers.
//!
//! Offline by default. The ONLY probe that touches the network is the OpenAI
//! key validation, and only when not in offline mode. Network classification
//! is split into the pure `openai_probe_from_keyprobe` so probes are
//! unit-testable without a live endpoint.

use std::path::Path;

use anthropic_oauth::{load_from_source, locate_credentials};
use chrono::{DateTime, Utc};
use watcher::KeyProbe;

/// Severity of a single probe. Ordered Ok < Warn < Fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckLevel {
    Ok,
    Warn,
    Fail,
}

impl CheckLevel {
    /// The more-severe of two levels (folds a probe set to one worst level).
    pub fn worst(self, other: CheckLevel) -> CheckLevel {
        use CheckLevel::*;
        match (self, other) {
            (Fail, _) | (_, Fail) => Fail,
            (Warn, _) | (_, Warn) => Warn,
            _ => Ok,
        }
    }
}

/// What kind of failure a probe represents, so the exit-code mapping can
/// distinguish auth (exit 3) from network (exit 4) per spec §9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckCategory {
    Auth,
    Network,
    Other,
}

/// One probe's outcome: severity, category, a one-line message, and an
/// optional actionable hint. No secret material ever lands here.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub level: CheckLevel,
    pub category: CheckCategory,
    pub message: String,
    pub hint: Option<String>,
}

impl CheckResult {
    pub fn ok(category: CheckCategory, message: impl Into<String>) -> Self {
        CheckResult {
            level: CheckLevel::Ok,
            category,
            message: message.into(),
            hint: None,
        }
    }
    pub fn warn(category: CheckCategory, message: impl Into<String>, hint: Option<String>) -> Self {
        CheckResult {
            level: CheckLevel::Warn,
            category,
            message: message.into(),
            hint,
        }
    }
    pub fn fail(category: CheckCategory, message: impl Into<String>, hint: Option<String>) -> Self {
        CheckResult {
            level: CheckLevel::Fail,
            category,
            message: message.into(),
            hint,
        }
    }
}

/// Strict 'is it expired right now' check: now >= expires (zero margin).
/// Mirrors the private `token_needs_refresh` in sources.rs / oauth_poll.rs
/// with a zero margin (those use a 300s near-expiry band; doctor wants a hard
/// expiry answer). Reimplemented here because that fn is not exported.
fn token_expired(expires_at_ms: i64, now: DateTime<Utc>) -> bool {
    now.timestamp_millis() >= expires_at_ms
}

/// Pure OAuth severity rule, factored out of `probe_claude_oauth` so the
/// expired/read-only matrix is unit-testable without a real credential.
///
/// - File source, valid       -> Ok
/// - File source, expired     -> Warn (Balanze owns the file; the poller refreshes it)
/// - Keychain source, valid   -> Ok
/// - Keychain source, expired -> Fail + Auth (CredentialExpiredReadOnly: re-run `claude login`)
fn oauth_check_from_parts(
    writable: bool,
    expires_at_ms: i64,
    now: DateTime<Utc>,
    location: &str,
) -> CheckResult {
    let expired = token_expired(expires_at_ms, now);
    match (writable, expired) {
        (_, false) => CheckResult::ok(
            CheckCategory::Auth,
            format!("Claude OAuth credential found and valid ({location})"),
        ),
        (true, true) => CheckResult::warn(
            CheckCategory::Auth,
            format!("Claude OAuth token expired ({location})"),
            Some("Balanze will refresh it on the next poll; no action needed.".to_string()),
        ),
        (false, true) => CheckResult::fail(
            CheckCategory::Auth,
            format!("Claude OAuth token expired and read-only ({location})"),
            Some(
                "re-run `claude login` in Claude Code (Balanze cannot refresh a credential it does not own)."
                    .to_string(),
            ),
        ),
    }
}

/// Pure mapping from a network `KeyProbe` to a `CheckResult`, so the OpenAI
/// validation branch is testable without a live request. `Valid` -> Ok;
/// `Rejected` (401/403) -> Fail+Auth; `Unreachable` -> Warn+Network (transient;
/// the poller may still succeed later).
fn openai_probe_from_keyprobe(probe: KeyProbe) -> CheckResult {
    match probe {
        KeyProbe::Valid => CheckResult::ok(CheckCategory::Auth, "OpenAI key present and validated"),
        KeyProbe::Rejected(msg) => CheckResult::fail(
            CheckCategory::Auth,
            format!("OpenAI key rejected: {msg}"),
            Some(
                "Generate an admin key at https://platform.openai.com/settings/organization/admin-keys"
                    .to_string(),
            ),
        ),
        KeyProbe::Unreachable(msg) => CheckResult::warn(
            CheckCategory::Network,
            format!("OpenAI key present but could not be validated: {msg}"),
            Some(
                "Re-run `balanze-cli doctor` when the network is back, or use --offline to skip."
                    .to_string(),
            ),
        ),
    }
}

/// Fold a probe set to a process exit code per spec §9. A Fail wins (auth -> 3,
/// network -> 4, other -> 1); auth is preferred over network when both fail
/// (the more actionable blocker). No Fail: Warn maps to 5 only under --strict
/// (degraded), else 0.
///
// TODO: when PR5's `exit.rs` lands, swap this body to return `exit::ExitClass`
// and let the caller call `.code()`. This module keeps the raw i32 mapping so
// doctor is mergeable before PR5; the numeric values are identical.
pub fn worst_exit_code(results: &[CheckResult], strict: bool) -> i32 {
    let mut has_warn = false;
    let mut fail: Option<CheckCategory> = None;
    for r in results {
        match r.level {
            CheckLevel::Fail => {
                // Auth fail is reported even if a later network fail exists.
                if r.category == CheckCategory::Auth {
                    fail = Some(CheckCategory::Auth);
                } else {
                    fail.get_or_insert(r.category);
                }
            }
            CheckLevel::Warn => has_warn = true,
            CheckLevel::Ok => {}
        }
    }
    if let Some(cat) = fail {
        return match cat {
            CheckCategory::Auth => 3,
            CheckCategory::Network => 4,
            CheckCategory::Other => 1,
        };
    }
    if has_warn && strict {
        return 5;
    }
    0
}

/// Probe 1: Claude OAuth credential. Locates the source (file vs macOS
/// Keychain), confirms a real read (so the optimistic macOS Keychain source
/// does not false-positive; the read may prompt once), and applies the
/// expiry/read-only rule.
///
/// Severity: a genuinely-absent credential (Claude Code not installed or not
/// logged in) is WARN, not Fail - the app treats a not-configured source as
/// neutral (SourceUnavailable), and other providers can still populate the
/// matrix. Only an actual breakage is Fail: a credential that is present but
/// unreadable, or the keychain expired-read-only case in `oauth_check_from_parts`.
pub fn probe_claude_oauth(now: DateTime<Utc>) -> CheckResult {
    let source = match locate_credentials() {
        Ok(s) => s,
        Err(_) => {
            return CheckResult::warn(
                CheckCategory::Auth,
                "Claude OAuth not configured (Claude Code not installed or not logged in)",
                Some(
                    "run `claude login` in Claude Code to enable the Claude subscription-quota cell"
                        .into(),
                ),
            );
        }
    };
    let creds = match load_from_source(&source) {
        Ok(c) => c,
        Err(e) => {
            return CheckResult::fail(
                CheckCategory::Auth,
                format!(
                    "Claude OAuth credential present but unreadable ({})",
                    source.describe()
                ),
                Some(format!("{e}")),
            );
        }
    };
    let writable = source.writable_path().is_some();
    oauth_check_from_parts(
        writable,
        creds.claude_ai_oauth.expires_at,
        now,
        &source.describe(),
    )
}

/// Probe 2: Claude JSONL projects dir(s) + file count. An empty dir vec means
/// no projects dir anywhere (Warn: the API-cost cell will be empty, not fatal).
pub fn probe_claude_jsonl() -> CheckResult {
    let dirs = claude_parser::find_all_claude_projects_dirs();
    if dirs.is_empty() {
        return CheckResult::warn(
            CheckCategory::Other,
            "No Claude Code JSONL projects directory found",
            Some(
                "Claude API cost is derived from ~/.claude/projects/**/*.jsonl; none present."
                    .to_string(),
            ),
        );
    }
    let mut file_count = 0usize;
    for dir in &dirs {
        if let Ok(files) = claude_parser::find_jsonl_files(dir) {
            file_count += files.len();
        }
    }
    if file_count == 0 {
        return CheckResult::warn(
            CheckCategory::Other,
            format!(
                "Claude projects dir(s) found ({}) but no JSONL session files",
                dirs.len()
            ),
            Some("Use Claude Code once to populate session history.".to_string()),
        );
    }
    CheckResult::ok(
        CheckCategory::Other,
        format!(
            "Claude JSONL: {} session file(s) across {} dir(s)",
            file_count,
            dirs.len()
        ),
    )
}

/// Probe 3: Codex sessions presence + latest rollout age. `FileMissing` from
/// `find_codex_sessions_dir` means 'not installed' (Warn). A dir with no
/// sessions yet is Warn. A latest session reports its mtime-derived age.
pub fn probe_codex() -> CheckResult {
    let dir = match codex_local::find_codex_sessions_dir() {
        Err(codex_local::ParseError::FileMissing(_)) => {
            return CheckResult::warn(
                CheckCategory::Other,
                "Codex CLI not installed (no ~/.codex/sessions/)",
                Some("Install/run the Codex CLI to populate the Codex quota cell.".to_string()),
            );
        }
        Err(e) => {
            return CheckResult::fail(
                CheckCategory::Other,
                format!("Could not read Codex sessions dir: {e}"),
                None,
            );
        }
        Ok(d) => d,
    };
    match codex_local::find_latest_session(&dir) {
        Ok(Some(path)) => {
            let age = latest_session_age_label(&path);
            CheckResult::ok(
                CheckCategory::Other,
                format!("Codex sessions present; latest rollout {age}"),
            )
        }
        Ok(None) => CheckResult::warn(
            CheckCategory::Other,
            "Codex installed but no sessions yet",
            Some("Run `codex` once to record a session.".to_string()),
        ),
        Err(e) => CheckResult::fail(
            CheckCategory::Other,
            format!("Could not walk Codex sessions: {e}"),
            None,
        ),
    }
}

/// Human-readable age of the latest rollout file from its mtime. Best-effort:
/// an unreadable mtime falls back to "(age unknown)".
fn latest_session_age_label(path: &Path) -> String {
    let modified = std::fs::metadata(path).and_then(|m| m.modified());
    match modified.ok().and_then(|m| m.elapsed().ok()) {
        Some(elapsed) => {
            let secs = elapsed.as_secs();
            if secs < 90 {
                format!("{secs}s old")
            } else if secs < 5400 {
                format!("{}m old", secs / 60)
            } else if secs < 172_800 {
                format!("{}h old", secs / 3600)
            } else {
                format!("{}d old", secs / 86_400)
            }
        }
        None => "(age unknown)".to_string(),
    }
}

/// Probe 4 (offline): OpenAI key presence only. Honors BALANZE_OPENAI_KEY
/// precedence over the keychain (matching setup.rs / sources.rs). Returns the
/// resolved key (for the optional online step) plus a presence CheckResult.
/// Uses a single keychain `get` (exists is a get under the hood; avoids a
/// double ACL prompt on macOS).
pub fn probe_openai_key_presence() -> (Option<String>, CheckResult) {
    if let Ok(env_key) = std::env::var("BALANZE_OPENAI_KEY") {
        let trimmed = env_key.trim();
        if !trimmed.is_empty() {
            return (
                Some(trimmed.to_string()),
                CheckResult::ok(
                    CheckCategory::Auth,
                    "OpenAI key present (via BALANZE_OPENAI_KEY)",
                ),
            );
        }
    }
    match keychain::get(keychain::keys::OPENAI_API_KEY) {
        Ok(k) => (
            Some(k),
            CheckResult::ok(CheckCategory::Auth, "OpenAI key present (keychain)"),
        ),
        Err(keychain::KeychainError::NotFound(_)) => (
            None,
            CheckResult::warn(
                CheckCategory::Auth,
                "No OpenAI key configured",
                Some(
                    "Run `balanze-cli set-openai-key` or set BALANZE_OPENAI_KEY; the OpenAI cost cell will be empty without it."
                        .to_string(),
                ),
            ),
        ),
        Err(e) => (
            None,
            CheckResult::fail(
                CheckCategory::Other,
                format!("Keychain read failed: {e}"),
                Some(
                    "Use BALANZE_OPENAI_KEY as a fallback if the OS keychain is unavailable."
                        .to_string(),
                ),
            ),
        ),
    }
}

/// Probe 4 (online): validate a resolved key via watcher::validate_openai_key
/// (one fail-fast month-to-date request). ASYNC.
pub async fn probe_openai_key_online(key: &str) -> CheckResult {
    let probe = watcher::validate_openai_key(key).await;
    openai_probe_from_keyprobe(probe)
}

/// Probe 5: is Balanze's statusLine wired in Claude Code's settings.json?
pub fn probe_statusline() -> CheckResult {
    use claude_statusline::{
        WireStatus, default_settings_path, locate_settings_path, read_wire_status,
    };
    let path = match locate_settings_path() {
        Ok(p) => p,
        Err(_) => default_settings_path(),
    };
    match read_wire_status(&path) {
        Ok(WireStatus::WiredToBalanze) => CheckResult::ok(
            CheckCategory::Other,
            format!("statusLine wired to balanze-cli ({})", path.display()),
        ),
        Ok(WireStatus::OccupiedBy(cmd)) => CheckResult::warn(
            CheckCategory::Other,
            format!("statusLine occupied by another command: {cmd}"),
            Some(format!(
                "Set statusLine.command to `balanze-cli statusline` in {} to use Balanze.",
                path.display()
            )),
        ),
        Ok(WireStatus::Unwired) => CheckResult::warn(
            CheckCategory::Other,
            "statusLine not wired",
            Some("Run `balanze-cli setup` to wire the Claude Code statusLine.".to_string()),
        ),
        Err(e) => CheckResult::fail(
            CheckCategory::Other,
            format!("Could not read Claude Code settings.json ({e})"),
            None,
        ),
    }
}

/// Probe 6: settings.json readable + keychain backend functional. `settings::load`
/// returns defaults when the file is absent (not an error), so Malformed is the
/// only fail. The keychain backend is probed via a benign read of the OpenAI key
/// entry: NotFound is a healthy backend (entry simply absent); a PlatformError
/// means the backend itself is broken.
pub fn probe_settings_and_keychain() -> CheckResult {
    let settings_label = settings::default_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unresolved config dir)".to_string());
    if let Err(e) = settings::load() {
        return CheckResult::fail(
            CheckCategory::Other,
            format!("settings.json unreadable: {e}"),
            Some(format!("Fix or remove {settings_label}.")),
        );
    }
    match keychain::get(keychain::keys::OPENAI_API_KEY) {
        Ok(_) | Err(keychain::KeychainError::NotFound(_)) => CheckResult::ok(
            CheckCategory::Other,
            format!("settings.json readable ({settings_label}); keychain backend OK"),
        ),
        Err(keychain::KeychainError::PlatformError { reason, .. }) => CheckResult::fail(
            CheckCategory::Other,
            format!("Keychain backend not functional: {reason}"),
            Some(
                "On Linux no native keychain is wired; use BALANZE_OPENAI_KEY instead.".to_string(),
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use watcher::KeyProbe;

    // Fixed now, mirroring the json_output.rs test convention.
    fn fixed_now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap()
    }

    #[test]
    fn oauth_file_valid_token_is_ok() {
        let now = fixed_now();
        let expires = now.timestamp_millis() + 3_600_000;
        let r = oauth_check_from_parts(/* writable */ true, expires, now, "the file path");
        assert_eq!(r.level, CheckLevel::Ok);
        assert_eq!(r.category, CheckCategory::Auth);
    }

    #[test]
    fn oauth_file_expired_token_is_warn_refreshable() {
        // A FILE source expired is refreshable, not fatal: Balanze owns the
        // file and the poller refreshes it. Warn, not Fail.
        let now = fixed_now();
        let expires = now.timestamp_millis() - 1;
        let r = oauth_check_from_parts(true, expires, now, "the file path");
        assert_eq!(r.level, CheckLevel::Warn);
    }

    #[test]
    fn oauth_keychain_expired_token_is_fail_readonly() {
        // macOS Keychain (writable_path == None) + expired -> CredentialExpiredReadOnly:
        // Fail + Auth, hint must name `claude login`.
        let now = fixed_now();
        let expires = now.timestamp_millis() - 1;
        let r = oauth_check_from_parts(/* writable */ false, expires, now, "keychain");
        assert_eq!(r.level, CheckLevel::Fail);
        assert_eq!(r.category, CheckCategory::Auth);
        assert!(
            r.hint
                .as_deref()
                .unwrap_or_default()
                .contains("claude login"),
            "hint must point at `claude login`: {:?}",
            r.hint
        );
    }

    #[test]
    fn oauth_keychain_valid_token_is_ok() {
        let now = fixed_now();
        let expires = now.timestamp_millis() + 3_600_000;
        let r = oauth_check_from_parts(false, expires, now, "keychain");
        assert_eq!(r.level, CheckLevel::Ok);
    }

    #[test]
    fn openai_keyprobe_valid_is_ok() {
        let r = openai_probe_from_keyprobe(KeyProbe::Valid);
        assert_eq!(r.level, CheckLevel::Ok);
        assert_eq!(r.category, CheckCategory::Auth);
    }

    #[test]
    fn openai_keyprobe_rejected_is_fail_auth() {
        let r = openai_probe_from_keyprobe(KeyProbe::Rejected("bad key (401)".into()));
        assert_eq!(r.level, CheckLevel::Fail);
        assert_eq!(r.category, CheckCategory::Auth);
        assert!(r.message.contains("401"));
    }

    #[test]
    fn openai_keyprobe_unreachable_is_warn_network() {
        let r = openai_probe_from_keyprobe(KeyProbe::Unreachable("network down".into()));
        assert_eq!(r.level, CheckLevel::Warn);
        assert_eq!(r.category, CheckCategory::Network);
    }

    #[test]
    fn level_worst_reduces_to_highest_severity() {
        assert_eq!(CheckLevel::Ok.worst(CheckLevel::Warn), CheckLevel::Warn);
        assert_eq!(CheckLevel::Warn.worst(CheckLevel::Fail), CheckLevel::Fail);
        assert_eq!(CheckLevel::Fail.worst(CheckLevel::Ok), CheckLevel::Fail);
    }

    #[test]
    fn worst_exit_code_maps_per_spec_table() {
        let oks = vec![
            CheckResult::ok(CheckCategory::Other, "a"),
            CheckResult::ok(CheckCategory::Auth, "b"),
        ];
        assert_eq!(worst_exit_code(&oks, /* strict */ false), 0);

        let auth = vec![CheckResult::fail(CheckCategory::Auth, "x", None)];
        assert_eq!(worst_exit_code(&auth, false), 3);

        let net = vec![CheckResult::fail(CheckCategory::Network, "x", None)];
        assert_eq!(worst_exit_code(&net, false), 4);

        let warn = vec![CheckResult::warn(CheckCategory::Other, "x", None)];
        assert_eq!(worst_exit_code(&warn, false), 0);
        assert_eq!(worst_exit_code(&warn, true), 5);
    }
}
