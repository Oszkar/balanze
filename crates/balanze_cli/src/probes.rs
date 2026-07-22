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

use crate::exit::ExitClass;

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

/// Pure OAuth severity rule, factored out of `probe_claude_oauth` so the
/// read-only expiry rule is unit-testable without a real credential.
///
/// - Any source, valid   -> Ok
/// - Any source, expired -> Fail + Auth (re-run `claude login`)
fn oauth_check_from_parts(expired: bool, location: &str) -> CheckResult {
    if expired {
        CheckResult::fail(
            CheckCategory::Auth,
            format!("Claude OAuth token expired and read-only ({location})"),
            Some(
                "re-run `claude login` in Claude Code (Balanze never modifies Claude Code credentials)."
                    .to_string(),
            ),
        )
    } else {
        CheckResult::ok(
            CheckCategory::Auth,
            format!("Claude OAuth credential found and valid ({location})"),
        )
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

/// Fold a probe set to an [`ExitClass`]. A Fail wins (auth, then network, then
/// other); auth is preferred over network when both fail (the more actionable
/// blocker). No Fail: Warn maps to `Degraded` only under --strict, else `Ok`.
///
/// Returns the shared `exit::ExitClass` so `doctor` and the `status` path agree
/// on one taxonomy; `.code()` yields auth=3, network=4, other=1,
/// strict-degraded=5, clean=0.
pub fn worst_exit_code(results: &[CheckResult], strict: bool) -> ExitClass {
    // Track each failing category independently so the result is ORDER-
    // INDEPENDENT: an Other fail seen before a Network fail must still rank the
    // Network fail above Other. Ranking is Auth > Network > Other.
    let mut has_warn = false;
    let mut has_auth_fail = false;
    let mut has_network_fail = false;
    let mut has_other_fail = false;
    for r in results {
        match r.level {
            CheckLevel::Fail => match r.category {
                CheckCategory::Auth => has_auth_fail = true,
                CheckCategory::Network => has_network_fail = true,
                CheckCategory::Other => has_other_fail = true,
            },
            CheckLevel::Warn => has_warn = true,
            CheckLevel::Ok => {}
        }
    }
    if has_auth_fail {
        return ExitClass::AuthMissing;
    }
    if has_network_fail {
        return ExitClass::Network;
    }
    if has_other_fail {
        return ExitClass::Other;
    }
    if has_warn && strict {
        return ExitClass::Degraded;
    }
    ExitClass::Ok
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
    oauth_check_from_parts(creds.claude_ai_oauth.is_expired_at(now), &source.describe())
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
    let mut read_errors = Vec::new();
    for dir in &dirs {
        match claude_parser::find_jsonl_files(dir) {
            Ok(files) => file_count += files.len(),
            Err(e) => read_errors.push(format!("{}: {e}", dir.display())),
        }
    }
    if !read_errors.is_empty() {
        return CheckResult::fail(
            CheckCategory::Other,
            format!(
                "Could not read Claude JSONL projects dir(s): {}",
                read_errors.join("; ")
            ),
            Some(
                "Fix directory permissions or remove stale Claude projects directories."
                    .to_string(),
            ),
        );
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

/// What the OpenAI presence probe learned about the keychain backend, threaded
/// into [`probe_settings_and_keychain`] so a doctor run never does a SECOND
/// `keychain::get(OPENAI_API_KEY)` (a second macOS ACL prompt). The presence
/// probe already does the read whenever `BALANZE_OPENAI_KEY` does not short-
/// circuit it; that one read settles the backend's health too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeychainHealth {
    /// The keychain backend responded (Ok or NotFound on the get) - reachable.
    Healthy,
    /// A PlatformError on the get: the backend itself is not functional. Carries
    /// the reason (never any secret value).
    Broken(String),
    /// This platform wires no credential store at all (Linux). Not a fault: the
    /// documented path there is the BALANZE_OPENAI_KEY env var, so doctor
    /// reports it as a warning with guidance rather than a failed check.
    NotAvailable,
    /// No credential store, AND the env var is already supplying the key - so
    /// there is nothing for the user to do. Distinct from [`Self::NotAvailable`]
    /// because that one's guidance ("set BALANZE_OPENAI_KEY") would be telling
    /// the user to do what they have already done, and a warning here would
    /// make doctor claim degradation on a box that is fully working.
    NotAvailableKeyFromEnv,
    /// The presence probe short-circuited on `BALANZE_OPENAI_KEY` and never
    /// touched the keychain, so its health is still unknown. The settings probe
    /// performs the (single) read in this case.
    NotProbed,
}

/// Probe 4 (offline): OpenAI key presence only. Honors BALANZE_OPENAI_KEY
/// precedence over the keychain (matching setup.rs / sources.rs). Returns the
/// resolved key (for the optional online step), a presence CheckResult, and the
/// keychain backend health learned from the (at most one) `get` it performs.
/// Uses a single keychain `get` (exists is a get under the hood; avoids a
/// double ACL prompt on macOS), and the `KeychainHealth` it returns lets the
/// settings/keychain probe avoid a SECOND `get`.
pub fn probe_openai_key_presence() -> (Option<String>, CheckResult, KeychainHealth) {
    if let Ok(env_key) = std::env::var("BALANZE_OPENAI_KEY") {
        let trimmed = env_key.trim();
        if !trimmed.is_empty() {
            return (
                Some(trimmed.to_string()),
                CheckResult::ok(
                    CheckCategory::Auth,
                    "OpenAI key present (via BALANZE_OPENAI_KEY)",
                ),
                // Did not touch the keychain - leave its health for the settings
                // probe to settle with the single allowed read.
                KeychainHealth::NotProbed,
            );
        }
    }
    match keychain::get(keychain::keys::OPENAI_API_KEY) {
        Ok(k) => (
            Some(k),
            CheckResult::ok(CheckCategory::Auth, "OpenAI key present (keychain)"),
            KeychainHealth::Healthy,
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
            KeychainHealth::Healthy,
        ),
        Err(keychain::KeychainError::PlatformError { reason, .. }) => (
            None,
            CheckResult::fail(
                CheckCategory::Other,
                format!("Keychain read failed: {reason}"),
                Some(
                    "Use BALANZE_OPENAI_KEY as a fallback if the OS keychain is unavailable."
                        .to_string(),
                ),
            ),
            KeychainHealth::Broken(reason),
        ),
        Err(keychain::KeychainError::NoStore) => (
            None,
            CheckResult::warn(
                CheckCategory::Auth,
                "No OpenAI key configured (this platform has no credential store)",
                Some(keychain::NO_STORE_HINT.to_string()),
            ),
            KeychainHealth::NotAvailable,
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
        // Show the occupying command so the user knows what currently owns the
        // statusLine slot - it is their own local config surfaced on their own
        // terminal, and naming it is useful for the diagnostic.
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

/// Resolve the keychain backend health WITHOUT a redundant read. If the OpenAI
/// presence probe already touched the keychain (`Healthy` / `Broken`), reuse
/// that result; only when it short-circuited on the env var (`NotProbed`) do we
/// perform the single allowed `keychain::get` here. This keeps a doctor run to
/// at most one `keychain::get(OPENAI_API_KEY)` (one macOS ACL prompt).
fn resolve_keychain_health(prior: KeychainHealth) -> KeychainHealth {
    match prior {
        KeychainHealth::Healthy
        | KeychainHealth::Broken(_)
        | KeychainHealth::NotAvailable
        | KeychainHealth::NotAvailableKeyFromEnv => prior,
        // `NotProbed` is returned only when the presence probe short-circuited
        // on a NON-EMPTY BALANZE_OPENAI_KEY, so a key is already configured.
        // Where there is no store, the read below can only tell us what
        // `has_native_store()` already knows - so skip it. That also avoids a
        // guaranteed-pointless macOS ACL prompt if a store is ever wired on a
        // platform that currently has none.
        KeychainHealth::NotProbed if !keychain::has_native_store() => {
            KeychainHealth::NotAvailableKeyFromEnv
        }
        KeychainHealth::NotProbed => match keychain::get(keychain::keys::OPENAI_API_KEY) {
            Ok(_) | Err(keychain::KeychainError::NotFound(_)) => KeychainHealth::Healthy,
            Err(keychain::KeychainError::PlatformError { reason, .. }) => {
                KeychainHealth::Broken(reason)
            }
            Err(keychain::KeychainError::NoStore) => KeychainHealth::NotAvailable,
        },
    }
}

/// Probe 6: settings.json readable + keychain backend functional. `settings::load`
/// returns defaults when the file is absent (not an error), so Malformed is the
/// only fail. The keychain backend health is taken from the OpenAI presence
/// probe when it already read the keychain (NotFound/Ok => healthy, PlatformError
/// => broken); only the env-var short-circuit case (`NotProbed`) triggers the
/// single read here, so a doctor run does at most one keychain get.
pub fn probe_settings_and_keychain(keychain_health: KeychainHealth) -> CheckResult {
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
    match resolve_keychain_health(keychain_health) {
        KeychainHealth::Healthy => CheckResult::ok(
            CheckCategory::Other,
            format!("settings.json readable ({settings_label}); keychain backend OK"),
        ),
        KeychainHealth::Broken(reason) => CheckResult::fail(
            CheckCategory::Other,
            format!("Keychain backend not functional: {reason}"),
            Some(keychain::NO_STORE_HINT.to_string()),
        ),
        // Ok, not Warn: the platform limitation is real but fully mitigated, so
        // there is nothing to act on and no hint to give. A Warn here would make
        // doctor report degradation on a box where everything works.
        KeychainHealth::NotAvailableKeyFromEnv => CheckResult::ok(
            CheckCategory::Other,
            format!(
                "settings.json readable ({settings_label}); no OS credential store, OpenAI key supplied via BALANZE_OPENAI_KEY"
            ),
        ),
        KeychainHealth::NotAvailable => CheckResult::warn(
            CheckCategory::Other,
            format!(
                "settings.json readable ({settings_label}); no OS credential store on this platform"
            ),
            Some(keychain::NO_STORE_HINT.to_string()),
        ),
        // resolve_keychain_health never returns NotProbed; exhaustive for safety.
        KeychainHealth::NotProbed => CheckResult::ok(
            CheckCategory::Other,
            format!("settings.json readable ({settings_label}); keychain backend OK"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use watcher::KeyProbe;

    #[test]
    fn oauth_file_valid_token_is_ok() {
        let r = oauth_check_from_parts(false, "the file path");
        assert_eq!(r.level, CheckLevel::Ok);
        assert_eq!(r.category, CheckCategory::Auth);
    }

    #[test]
    fn oauth_file_expired_token_is_fail_readonly() {
        let r = oauth_check_from_parts(true, "the file path");
        assert_eq!(r.level, CheckLevel::Fail);
        assert_eq!(r.category, CheckCategory::Auth);
        assert!(
            r.hint
                .as_deref()
                .unwrap_or_default()
                .contains("claude login")
        );
    }

    #[test]
    fn oauth_keychain_expired_token_is_fail_readonly() {
        let r = oauth_check_from_parts(true, "keychain");
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
        let r = oauth_check_from_parts(false, "keychain");
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
        use crate::exit::ExitClass;

        let oks = vec![
            CheckResult::ok(CheckCategory::Other, "a"),
            CheckResult::ok(CheckCategory::Auth, "b"),
        ];
        assert_eq!(worst_exit_code(&oks, /* strict */ false), ExitClass::Ok);

        let auth = vec![CheckResult::fail(CheckCategory::Auth, "x", None)];
        assert_eq!(worst_exit_code(&auth, false), ExitClass::AuthMissing);

        let net = vec![CheckResult::fail(CheckCategory::Network, "x", None)];
        assert_eq!(worst_exit_code(&net, false), ExitClass::Network);

        let warn = vec![CheckResult::warn(CheckCategory::Other, "x", None)];
        assert_eq!(worst_exit_code(&warn, false), ExitClass::Ok);
        assert_eq!(worst_exit_code(&warn, true), ExitClass::Degraded);
        // The numeric contract still holds through .code().
        assert_eq!(worst_exit_code(&warn, true).code(), 5);
    }

    #[test]
    fn worst_exit_code_auth_outranks_network() {
        use crate::exit::ExitClass;

        // Auth outranks Network regardless of order in the slice.
        let auth_first = vec![
            CheckResult::fail(CheckCategory::Auth, "a", None),
            CheckResult::fail(CheckCategory::Network, "n", None),
        ];
        assert_eq!(worst_exit_code(&auth_first, false), ExitClass::AuthMissing);
        let net_first = vec![
            CheckResult::fail(CheckCategory::Network, "n", None),
            CheckResult::fail(CheckCategory::Auth, "a", None),
        ];
        assert_eq!(worst_exit_code(&net_first, false), ExitClass::AuthMissing);
    }

    #[test]
    fn worst_exit_code_is_order_independent_for_other_then_network() {
        use crate::exit::ExitClass;

        // Regression: an Other fail seen BEFORE a Network fail must still rank
        // Network above Other. The old Option-accumulation returned Other.
        let other_then_network = vec![
            CheckResult::fail(CheckCategory::Other, "o", None),
            CheckResult::fail(CheckCategory::Network, "n", None),
        ];
        assert_eq!(
            worst_exit_code(&other_then_network, false),
            ExitClass::Network
        );
        // And the reverse order agrees.
        let network_then_other = vec![
            CheckResult::fail(CheckCategory::Network, "n", None),
            CheckResult::fail(CheckCategory::Other, "o", None),
        ];
        assert_eq!(
            worst_exit_code(&network_then_other, false),
            ExitClass::Network
        );
    }

    #[test]
    fn resolve_keychain_health_reuses_prior_signal_without_a_read() {
        // A prior Healthy/Broken signal from the presence probe is returned
        // verbatim - no second keychain::get (the one-prompt-max invariant). Only
        // the NotProbed case (env-var short-circuit) reads, which we do not
        // exercise here to keep the test environment-free.
        assert_eq!(
            resolve_keychain_health(KeychainHealth::Healthy),
            KeychainHealth::Healthy
        );
        let broken = KeychainHealth::Broken("backend down".to_string());
        assert_eq!(resolve_keychain_health(broken.clone()), broken);
    }

    #[test]
    fn settings_probe_reuses_broken_health_as_fail_without_reading() {
        // When the presence probe already saw a PlatformError, the settings
        // probe surfaces a Fail using that signal - no second keychain read.
        let r = probe_settings_and_keychain(KeychainHealth::Broken("keychain locked".to_string()));
        assert_eq!(r.level, CheckLevel::Fail);
        assert_eq!(r.category, CheckCategory::Other);
        assert!(
            r.message.contains("keychain locked"),
            "reason must surface: {}",
            r.message
        );
    }

    // -----------------------------------------------------------------------
    // FIX E(4): Other-category Fail -> exit code 1 (previously untested)
    // -----------------------------------------------------------------------

    #[test]
    fn other_fail_exits_one() {
        use crate::exit::ExitClass;

        // An Other-category Fail (e.g., settings.json unreadable, codex parse
        // error) must map to `Other` (exit code 1), not Ok or any auth/network
        // class. This branch in worst_exit_code was previously uncovered.
        let results = vec![CheckResult::fail(
            CheckCategory::Other,
            "settings.json unreadable",
            None,
        )];
        assert_eq!(
            worst_exit_code(&results, /* strict */ false),
            ExitClass::Other,
            "Other-category Fail must map to ExitClass::Other (code 1)"
        );
        assert_eq!(worst_exit_code(&results, false).code(), 1);
    }

    #[test]
    fn other_fail_outranked_by_auth_and_network() {
        use crate::exit::ExitClass;

        // Auth > Network > Other. A mix of Other + Network still returns
        // Network; Other + Auth still returns AuthMissing.
        let other_plus_network = vec![
            CheckResult::fail(CheckCategory::Other, "o", None),
            CheckResult::fail(CheckCategory::Network, "n", None),
        ];
        assert_eq!(
            worst_exit_code(&other_plus_network, false),
            ExitClass::Network
        );

        let other_plus_auth = vec![
            CheckResult::fail(CheckCategory::Other, "o", None),
            CheckResult::fail(CheckCategory::Auth, "a", None),
        ];
        assert_eq!(
            worst_exit_code(&other_plus_auth, false),
            ExitClass::AuthMissing
        );
    }

    #[test]
    fn no_store_health_is_a_warning_not_a_failure() {
        // A storeless platform is a documented condition, not a broken backend.
        // doctor must not tell a Linux user something is wrong with their machine.
        let result = probe_settings_and_keychain(KeychainHealth::NotAvailable);
        assert_eq!(
            result.level,
            CheckLevel::Warn,
            "a storeless platform must warn, not fail"
        );
        assert!(
            result
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("BALANZE_OPENAI_KEY")),
            "the hint must name the documented env override"
        );
    }

    #[test]
    fn env_supplied_key_needs_no_hint_about_setting_the_env_var() {
        // The user has already set BALANZE_OPENAI_KEY - that is the only way to
        // reach this state. Telling them to set it is noise, and a Warn would
        // make doctor claim degradation on a box that is fully working.
        let result = probe_settings_and_keychain(KeychainHealth::NotAvailableKeyFromEnv);
        assert_eq!(result.level, CheckLevel::Ok);
        assert!(
            result.hint.is_none(),
            "nothing to act on, so no hint: {:?}",
            result.hint
        );
        assert!(
            result.message.contains("BALANZE_OPENAI_KEY"),
            "say where the key came from: {}",
            result.message
        );
    }

    /// Only compiled where there is genuinely no store, which is the platform
    /// the short-circuit exists for.
    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn storeless_platform_with_env_key_resolves_without_reading_the_keychain() {
        assert_eq!(
            resolve_keychain_health(KeychainHealth::NotProbed),
            KeychainHealth::NotAvailableKeyFromEnv
        );
    }

    #[test]
    fn not_available_health_survives_resolution_without_a_second_read() {
        // NotAvailable is already settled; resolving it must not trigger the
        // extra keychain::get that NotProbed does.
        assert_eq!(
            resolve_keychain_health(KeychainHealth::NotAvailable),
            KeychainHealth::NotAvailable
        );
    }
}
