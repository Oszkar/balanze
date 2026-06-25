//! Process-exit taxonomy for `balanze-cli`.
//!
//! `main` builds a `Snapshot` (or runs `doctor`) with `anyhow` at the
//! boundaries, then classifies the outcome ONCE and exits with the resulting
//! code. Keeping the mapping pure (no I/O, no process exit) makes every case
//! unit-testable.
//!
//! Codes (documented in `--help` and the README, AGENTS.md §9 / the v0.4.1
//! CLI-maturity design):
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0    | OK (degraded sources still exit 0 unless `--strict`) |
//! | 1    | unexpected / other |
//! | 2    | usage error (clap default) |
//! | 3    | auth / credentials missing or expired |
//! | 4    | network / provider unreachable |
//! | 5    | partial / degraded (only under `--strict`) |
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
    /// Credentials missing or expired (re-run `claude login`, or set the key).
    AuthMissing,
    /// A provider was unreachable (transport / timeout).
    Network,
    /// A source was stale or errored, surfaced as a failure only under
    /// `--strict`; otherwise the status still rendered and we exit `Ok`.
    Degraded,
}

impl ExitClass {
    /// The process exit code for this class. Stable, scripting-facing contract -
    /// do not renumber without updating `--help` and the README.
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

/// True if `err` reads as a missing-or-expired credential. Matches the
/// `CredentialExpiredReadOnly` copy ("re-run `claude login`"), the
/// credentials-missing wording, and generic expired/unauthorized text.
fn looks_like_auth(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("claude login")
        || e.contains("expired")
        || e.contains("credentials missing")
        || e.contains("re-run")
        || e.contains("unauthorized")
        || e.contains("not authenticated")
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
        assert_eq!(classify_snapshot(&snap, false), ExitClass::AuthMissing);
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
