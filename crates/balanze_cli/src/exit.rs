//! Process-exit taxonomy for `balanze-cli`.
//!
//! `main` builds a `Snapshot` (or runs `doctor`) with `anyhow` at the
//! boundaries, then classifies the outcome ONCE and exits with the resulting
//! code. Keeping the mapping pure (no I/O, no process exit) makes every case
//! unit-testable.
//!
//! Codes (also documented in `--help` and the README exit-code table - keep the
//! three in lockstep):
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0    | OK (degraded sources still exit 0 unless `--strict`) |
//! | 1    | unexpected / other |
//! | 2    | usage error (clap default) |
//! | 3    | auth / credentials expired or rejected |
//! | 4    | network / provider unreachable |
//! | 5    | partial / degraded (only under `--strict`) |
//!
//! Code 3 means a credential was found and refused. An ABSENT credential is not
//! an auth failure - see [`looks_like_auth`].
//!
//! The two surfaces are not perfectly symmetric on one case, which is why the
//! public contract states only what BOTH honor. `doctor` also returns 3 for a
//! credential that is present but unreadable (`probes::probe_claude_oauth`
//! types that Fail + Auth). `status` cannot: it classifies by substring, and an
//! unreadable credential surfaces as e.g. "io error reading ..." with no auth
//! marker, so it lands on degraded (0, or 5 under `--strict`) instead. Closing
//! that gap is the typed-category TODO on [`looks_like_auth`], which needs a
//! Snapshot schema change; until then `--help` and the README deliberately do
//! not advertise "unreadable" as code 3.
//!
//! This module owns the single numeric contract. The `status` path classifies
//! a built `Snapshot` via [`classify_snapshot`]; the `doctor` path folds its
//! probe set into an [`ExitClass`] via `probes::worst_exit_code`, which returns
//! this same type so the two surfaces share one taxonomy.

use state_coordinator::Snapshot;

/// Process-exit classification. `code()` is the single source of the numeric
/// contract; nothing else in the crate should hard-code these ints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    /// Clean run (or a degraded source without `--strict`).
    Ok,
    /// Unexpected failure (anyhow error at the `main` boundary).
    Other,
    /// Usage error. Reserved for documentation parity with clap, which emits
    /// this code itself on bad flags / unknown subcommands; `main` never
    /// produces it (clap owns its own exit). Never constructed outside tests by
    /// design, so it is allowed to be dead in the binary - keeping the variant
    /// makes the `code()` taxonomy and the `--help` table complete (codes 0..=5).
    #[allow(dead_code)]
    Usage,
    /// A credential was found and the provider refused it - expired or rejected
    /// (re-run `claude login`, or refresh the key). NOT an absent credential: an
    /// unconfigured provider is neutral and exits `Ok` (see [`looks_like_auth`]).
    /// The variant name predates that distinction. `doctor` additionally types a
    /// present-but-unreadable credential into this class; `status` cannot (see
    /// the module doc).
    AuthMissing,
    /// A provider was unreachable (transport / timeout).
    Network,
    /// A source was stale or errored, surfaced as a failure only under
    /// `--strict`; otherwise the status still rendered and we exit `Ok`.
    Degraded,
}

impl ExitClass {
    /// The process exit code for this class. Stable, scripting-facing contract -
    /// do not renumber without updating `--help` (and the README table once it
    /// lands).
    pub fn code(self) -> i32 {
        match self {
            ExitClass::Ok => 0,
            ExitClass::Other => 1,
            ExitClass::Usage => 2,
            ExitClass::AuthMissing => 3,
            ExitClass::Network => 4,
            ExitClass::Degraded => 5,
        }
    }
}

/// True if `err` reads as a transport / reachability failure rather than an
/// auth rejection. Substring heuristic over the error text the source crates
/// already produce (`openai_client` / `anthropic_oauth` map their `Network` and
/// timeout variants into these words). Deliberately conservative: an unmatched
/// error falls through to the degraded/other path, never to a false `Network`.
fn looks_like_network(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("unreachable")
        || e.contains("network")
        || e.contains("timed out")
        || e.contains("timeout")
        || e.contains("connection")
        || e.contains("dns")
}

/// True if `err` reads as an expired-or-rejected credential (exit 3). Substring
/// heuristic over the error copy the source crates emit:
///   - Claude OAuth expired / read-only: "expired", "re-run", "claude login"
///     (anthropic_oauth's AuthExpired / RefreshTokenMissing /
///     CredentialExpiredReadOnly).
///   - OpenAI admin-key rejection (HTTP 401/403): "http 401", "http 403",
///     "returned 403", "rejected" - covering both the raw `OpenAiError` Display
///     and the sources.rs override copy. This aligns the status path with the
///     doctor path, which types the same `KeyProbe::Rejected` (401/403) as Auth.
///
/// A genuinely ABSENT credential ("credentials file not found", from
/// `OAuthError::CredentialsMissing`) is deliberately NOT matched: a
/// not-configured source is neutral (exit 0; degraded under --strict), mirroring
/// doctor's Warn for an absent credential - the user may simply not use that
/// provider, which is not an auth failure.
// TODO: replace this substring sniffing with a typed error category carried in
// the Snapshot (mirroring probes::CheckCategory) so status and doctor share real
// types, not strings. That needs a Snapshot schema change (out of scope here).
fn looks_like_auth(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("claude login")
        || e.contains("expired")
        || e.contains("re-run")
        || e.contains("unauthorized")
        || e.contains("not authenticated")
        || e.contains("http 401")
        || e.contains("http 403")
        || e.contains("returned 403")
        || e.contains("rejected")
}

/// Classify a built `Snapshot` into an `ExitClass`.
///
/// Precedence (highest first): auth-missing (3) over network (4) over
/// degraded/ok. A degraded source (any data `*_error` slot set that is neither
/// auth nor network) only escalates to `Degraded` (5) under `strict`; otherwise
/// the status still rendered, so we exit `Ok` (0).
///
/// Only the five data-bearing error slots are consulted (the slots feeding the
/// 4-quadrant matrix). `claude_statusline_error` is deliberately excluded: the
/// statusLine is a separate frozen-contract surface populated by the live
/// watcher, never by the one-shot CLI snapshot, and it is not one of the status
/// view's data cells. `claude_oauth_unavailable` and `None` quota/value slots
/// are NEUTRAL "not configured" markers (Claude Code / Codex not installed),
/// NOT errors - they never move the exit code.
pub fn classify_snapshot(snap: &Snapshot, strict: bool) -> ExitClass {
    // The data-bearing error slots, in no significant order (the auth-vs-network
    // precedence below is what matters, not position within a tier).
    let errors: [Option<&str>; 5] = [
        snap.claude_oauth_error.as_deref(),
        snap.claude_jsonl_error.as_deref(),
        snap.anthropic_api_cost_error.as_deref(),
        snap.codex_quota_error.as_deref(),
        snap.openai_error.as_deref(),
    ];

    let mut any_error = false;
    let mut any_network = false;
    for slot in errors.into_iter().flatten() {
        any_error = true;
        if looks_like_auth(slot) {
            // Auth is the strongest signal - return immediately.
            return ExitClass::AuthMissing;
        }
        if looks_like_network(slot) {
            any_network = true;
        }
    }

    if any_network {
        return ExitClass::Network;
    }
    if any_error {
        return if strict {
            ExitClass::Degraded
        } else {
            ExitClass::Ok
        };
    }
    ExitClass::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn empty() -> Snapshot {
        Snapshot::empty(Utc::now())
    }

    #[test]
    fn codes_match_taxonomy() {
        assert_eq!(ExitClass::Ok.code(), 0);
        assert_eq!(ExitClass::Other.code(), 1);
        assert_eq!(ExitClass::Usage.code(), 2);
        assert_eq!(ExitClass::AuthMissing.code(), 3);
        assert_eq!(ExitClass::Network.code(), 4);
        assert_eq!(ExitClass::Degraded.code(), 5);
    }

    #[test]
    fn clean_snapshot_is_ok_regardless_of_strict() {
        let snap = empty();
        assert_eq!(classify_snapshot(&snap, false), ExitClass::Ok);
        assert_eq!(classify_snapshot(&snap, true), ExitClass::Ok);
    }

    #[test]
    fn expired_oauth_is_auth_missing() {
        let mut snap = empty();
        snap.claude_oauth_error = Some(
            "Anthropic token from the macOS login Keychain is expired, \
             and Balanze can't refresh a credential it doesn't own - \
             re-run `claude login` in Claude Code"
                .to_string(),
        );
        // Auth wins even without --strict.
        assert_eq!(classify_snapshot(&snap, false), ExitClass::AuthMissing);
        assert_eq!(classify_snapshot(&snap, true), ExitClass::AuthMissing);
    }

    #[test]
    fn unreachable_provider_is_network() {
        let mut snap = empty();
        snap.openai_error =
            Some("OpenAI admin costs: provider unreachable (timed out)".to_string());
        assert_eq!(classify_snapshot(&snap, false), ExitClass::Network);
        assert_eq!(classify_snapshot(&snap, true), ExitClass::Network);
    }

    #[test]
    fn auth_takes_precedence_over_network() {
        let mut snap = empty();
        snap.openai_error = Some("network unreachable".to_string());
        snap.claude_oauth_error = Some("token expired - re-run `claude login`".to_string());
        // Auth short-circuits before the strict/degraded fold, so the result is
        // AuthMissing for BOTH strict values (guards the loop-vs-fold ordering).
        assert_eq!(classify_snapshot(&snap, false), ExitClass::AuthMissing);
        assert_eq!(classify_snapshot(&snap, true), ExitClass::AuthMissing);
    }

    #[test]
    fn openai_rejected_admin_key_is_auth_missing() {
        // The exact strings sources.rs sets into openai_error on HTTP 401/403.
        // The status path must agree with the doctor path (KeyProbe::Rejected ->
        // Auth -> exit 3), not silently fall through to degraded/ok.
        let mut snap401 = empty();
        snap401.openai_error = Some(
            "OpenAI admin key rejected (HTTP 401). Run `balanze-cli set-openai-key` \
             with a fresh `sk-admin-...` key."
                .to_string(),
        );
        assert_eq!(classify_snapshot(&snap401, false), ExitClass::AuthMissing);
        assert_eq!(classify_snapshot(&snap401, true), ExitClass::AuthMissing);

        let mut snap403 = empty();
        snap403.openai_error = Some(
            "OpenAI returned 403. organization/costs requires an admin API key \
             (`sk-admin-...`), not a project or service-account key."
                .to_string(),
        );
        assert_eq!(classify_snapshot(&snap403, false), ExitClass::AuthMissing);
        assert_eq!(classify_snapshot(&snap403, true), ExitClass::AuthMissing);
    }

    #[test]
    fn absent_claude_credential_is_neutral_not_auth() {
        // A genuinely absent credential surfaces as "credentials file not found"
        // (OAuthError::CredentialsMissing). It must NOT be AuthMissing(3):
        // not-configured is neutral, matching doctor's Warn. Only --strict
        // escalates it to degraded(5). Guards the deliberate decision to not map
        // absent credentials to exit 3.
        let mut snap = empty();
        snap.claude_oauth_error = Some(
            "credentials file not found (looked at [\"~/.claude/.credentials.json\"])".to_string(),
        );
        assert_eq!(classify_snapshot(&snap, false), ExitClass::Ok);
        assert_eq!(classify_snapshot(&snap, true), ExitClass::Degraded);
    }

    #[test]
    fn strict_flips_degraded_snapshot_from_ok_to_degraded() {
        // A schema-drift JSONL parse error: neither auth nor network.
        let mut snap = empty();
        snap.claude_jsonl_error = Some("schema drift at line 12: unknown field".to_string());
        // Non-strict: status still rendered, exit 0.
        assert_eq!(classify_snapshot(&snap, false), ExitClass::Ok);
        // Strict: the same degraded snapshot becomes Degraded(5).
        assert_eq!(classify_snapshot(&snap, true), ExitClass::Degraded);
        assert_eq!(classify_snapshot(&snap, true).code(), 5);
    }

    #[test]
    fn statusline_error_does_not_move_the_exit_class() {
        // claude_statusline_error is intentionally excluded from the status
        // exit-code decision (it is not a data cell, and the one-shot CLI never
        // populates it). Setting it must NOT flip the class, even under strict.
        let mut snap = empty();
        snap.claude_statusline_error = Some("schema drift v2 in statusline payload".to_string());
        assert_eq!(classify_snapshot(&snap, false), ExitClass::Ok);
        assert_eq!(classify_snapshot(&snap, true), ExitClass::Ok);
    }

    #[test]
    fn neutral_not_configured_markers_do_not_move_the_exit_class() {
        // claude_oauth_unavailable is a NEUTRAL "Claude Code not installed"
        // marker, not an error - it must never escalate the exit code.
        let mut snap = empty();
        snap.claude_oauth_unavailable = Some("Claude Code not detected".to_string());
        assert_eq!(classify_snapshot(&snap, false), ExitClass::Ok);
        assert_eq!(classify_snapshot(&snap, true), ExitClass::Ok);
    }
}
