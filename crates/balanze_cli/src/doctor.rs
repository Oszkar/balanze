//! `doctor` subcommand: run each integration probe (probes.rs), print an
//! OK/WARN/FAIL line with an actionable hint, then a readiness summary. The
//! worst severity maps to a process exit code per the v0.4.1 spec (§9):
//! auth fail -> 3, network fail -> 4, degraded-under-strict -> 5, else 0/1.
//!
//! Offline by default for everything except the OpenAI key validation, which
//! is skipped under `--offline`. `--quiet` prints only non-OK probes.

use std::io::Write;

use anstream::{AutoStream, ColorChoice};
use anyhow::Result;
use chrono::Utc;
use owo_colors::OwoColorize;

use crate::cli::DoctorArgs;
use crate::probes::{self, CheckCategory, CheckLevel, CheckResult, KeychainHealth};

/// Pure exit-code fold for doctor. Delegates to the shared probes mapping so
/// doctor and any future caller agree on the taxonomy.
fn doctor_exit_code(results: &[CheckResult], strict: bool) -> i32 {
    probes::worst_exit_code(results, strict)
}

/// Which probe lines to print: under --quiet, only non-OK results.
fn visible_results(results: &[CheckResult], quiet: bool) -> Vec<&CheckResult> {
    results
        .iter()
        .filter(|r| !quiet || r.level != CheckLevel::Ok)
        .collect()
}

/// The four DATA-SOURCE probes, captured by name so the "is anything usable"
/// decision reads them explicitly rather than by fragile array index. statusLine
/// and settings/keychain are infra, not data sources, so they are excluded from
/// the aggregate check.
struct DataSourceResults {
    oauth: CheckResult,
    jsonl: CheckResult,
    codex: CheckResult,
    openai: CheckResult,
    /// Backend health learned from the OpenAI presence probe's (single) keychain
    /// read, threaded into the settings/keychain infra probe so a doctor run
    /// performs at most one `keychain::get(OPENAI_API_KEY)` (one macOS prompt).
    keychain_health: KeychainHealth,
}

/// True when NONE of the four data sources is usable (every result is non-Ok).
/// If even one is Ok, balanze has something to show, so this is false. Pure +
/// testable: takes already-computed results, touches no environment.
fn no_data_source_configured(data_results: &[&CheckResult]) -> bool {
    data_results.iter().all(|r| r.level != CheckLevel::Ok)
}

/// The synthetic aggregate FAIL appended when no data source is usable. Auth
/// category so `worst_exit_code` maps it to exit 3 (the actionable fix is
/// providing a credential / source). Factored out so the render path and the
/// tests share one definition.
fn no_provider_aggregate_fail() -> CheckResult {
    CheckResult::fail(
        CheckCategory::Auth,
        "No usable provider configured - balanze has nothing to show",
        Some(
            "set up at least one: run `claude login`, use Claude Code so JSONL exists, set the OpenAI key (`balanze-cli set-openai-key`), or run Codex"
                .into(),
        ),
    )
}

/// Run the four data-source probes (Claude OAuth, Claude JSONL, Codex, OpenAI
/// key). `--offline` validates the OpenAI key for presence only; otherwise it
/// issues one fail-fast network request via the watcher.
fn run_data_source_probes(offline: bool) -> DataSourceResults {
    let now = Utc::now();
    let oauth = probes::probe_claude_oauth(now);
    let jsonl = probes::probe_claude_jsonl();
    let codex = probes::probe_codex();

    let (key, presence, keychain_health) = probes::probe_openai_key_presence();
    let openai = match (offline, key) {
        (false, Some(key)) => {
            // A key resolved and we are online: one fail-fast request.
            match tokio::runtime::Runtime::new() {
                Ok(rt) => rt.block_on(probes::probe_openai_key_online(&key)),
                Err(e) => CheckResult::warn(
                    // A runtime-creation failure is a local/Other problem, not a
                    // network reachability issue - keep the category honest.
                    CheckCategory::Other,
                    format!("Could not start OpenAI validation runtime: {e}"),
                    Some("Re-run with --offline to skip network validation.".to_string()),
                ),
            }
        }
        // Offline, or no key to validate: the presence probe is the answer.
        _ => presence,
    };

    DataSourceResults {
        oauth,
        jsonl,
        codex,
        openai,
        keychain_health,
    }
}

/// Run all six probes plus, when no data source is usable, an appended
/// aggregate FAIL. The four data sources come first (in their fixed order),
/// then the two infra probes (statusLine, settings/keychain), then the
/// optional aggregate line so the user sees WHY the exit is non-zero.
fn run_probes(offline: bool) -> Vec<CheckResult> {
    let ds = run_data_source_probes(offline);

    // Decide before the bindings move into `results`.
    let none_configured = no_data_source_configured(&[&ds.oauth, &ds.jsonl, &ds.codex, &ds.openai]);

    let mut results = Vec::with_capacity(7);
    results.push(ds.oauth);
    results.push(ds.jsonl);
    results.push(ds.codex);
    results.push(ds.openai);
    results.push(probes::probe_statusline());
    // Reuse the keychain health learned by the OpenAI presence probe so the
    // settings probe does not issue a second keychain read (FIX 4 invariant).
    results.push(probes::probe_settings_and_keychain(ds.keychain_health));

    if none_configured {
        // Rendered as a FAIL line that survives --quiet so the non-zero exit is
        // explained to the user.
        results.push(no_provider_aggregate_fail());
    }

    results
}

/// Colored short label for a level. Padded to 4 chars so messages align.
fn label(level: CheckLevel) -> String {
    match level {
        CheckLevel::Ok => "OK  ".green().to_string(),
        CheckLevel::Warn => "WARN".yellow().to_string(),
        CheckLevel::Fail => "FAIL".red().to_string(),
    }
}

/// Entry point. Returns the process exit code (caller converts to ExitCode).
pub fn cmd_doctor(args: &DoctorArgs, quiet: bool, strict: bool, no_color: bool) -> Result<i32> {
    let results = run_probes(args.offline);

    // owo-colors always writes the SGR codes; the AutoStream decides whether
    // they survive. Never under the global --no-color flag; otherwise Auto,
    // which anstream resolves against NO_COLOR / CLICOLOR / TTY itself. Mirrors
    // the status path's choice rule so the two color surfaces behave the same.
    let choice = if no_color {
        ColorChoice::Never
    } else {
        ColorChoice::Auto
    };
    let mut out = AutoStream::new(std::io::stdout(), choice);
    if !quiet {
        let _ = writeln!(out, "Balanze doctor");
        let _ = writeln!(out, "==============");
    }
    for r in visible_results(&results, quiet) {
        let _ = writeln!(out, "[{}] {}", label(r.level), r.message);
        if let Some(hint) = &r.hint {
            let _ = writeln!(out, "       hint: {hint}");
        }
    }

    let worst = results
        .iter()
        .fold(CheckLevel::Ok, |acc, r| acc.worst(r.level));
    let summary = match worst {
        CheckLevel::Ok => "All integrations OK.".green().to_string(),
        CheckLevel::Warn => "Some integrations degraded (see WARN above)."
            .yellow()
            .to_string(),
        CheckLevel::Fail => "One or more integrations failed (see FAIL above)."
            .red()
            .to_string(),
    };
    let _ = writeln!(out, "\n{summary}");
    let _ = out.flush();

    Ok(doctor_exit_code(&results, strict))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probes::{CheckCategory, CheckLevel, CheckResult};

    #[test]
    fn all_ok_exits_zero() {
        let results = vec![
            CheckResult::ok(CheckCategory::Auth, "oauth ok"),
            CheckResult::ok(CheckCategory::Other, "jsonl ok"),
        ];
        assert_eq!(doctor_exit_code(&results, /* strict */ false), 0);
    }

    #[test]
    fn auth_fail_exits_three() {
        let results = vec![
            CheckResult::ok(CheckCategory::Other, "jsonl ok"),
            CheckResult::fail(CheckCategory::Auth, "oauth expired read-only", None),
        ];
        assert_eq!(doctor_exit_code(&results, false), 3);
    }

    #[test]
    fn network_fail_exits_four() {
        let results = vec![CheckResult::fail(
            CheckCategory::Network,
            "openai unreachable",
            None,
        )];
        assert_eq!(doctor_exit_code(&results, false), 4);
    }

    #[test]
    fn warn_only_is_zero_unless_strict() {
        let results = vec![CheckResult::warn(CheckCategory::Other, "no codex", None)];
        assert_eq!(doctor_exit_code(&results, false), 0);
        assert_eq!(doctor_exit_code(&results, true), 5);
    }

    #[test]
    fn quiet_filters_to_non_ok() {
        let results = vec![
            CheckResult::ok(CheckCategory::Other, "jsonl ok"),
            CheckResult::warn(CheckCategory::Other, "no codex", None),
            CheckResult::fail(CheckCategory::Auth, "oauth missing", None),
        ];
        let shown = visible_results(&results, /* quiet */ true);
        assert_eq!(shown.len(), 2, "quiet shows only warn+fail");
        assert!(shown.iter().all(|r| r.level != CheckLevel::Ok));
        assert_eq!(
            visible_results(&results, false).len(),
            3,
            "non-quiet shows all"
        );
    }

    #[test]
    fn no_data_source_configured_true_when_all_non_ok() {
        // Every data source absent / degraded -> nothing to show.
        let warn = CheckResult::warn(CheckCategory::Auth, "oauth not configured", None);
        let warn2 = CheckResult::warn(CheckCategory::Other, "no jsonl", None);
        let warn3 = CheckResult::warn(CheckCategory::Other, "no codex", None);
        let warn4 = CheckResult::warn(CheckCategory::Auth, "no key", None);
        assert!(no_data_source_configured(&[&warn, &warn2, &warn3, &warn4]));
    }

    #[test]
    fn no_data_source_configured_false_when_one_ok() {
        // A single usable source (here JSONL) means balanze has something.
        let warn = CheckResult::warn(CheckCategory::Auth, "oauth not configured", None);
        let ok = CheckResult::ok(CheckCategory::Other, "jsonl: 5 files");
        let warn3 = CheckResult::warn(CheckCategory::Other, "no codex", None);
        let warn4 = CheckResult::warn(CheckCategory::Auth, "no key", None);
        assert!(!no_data_source_configured(&[&warn, &ok, &warn3, &warn4]));
    }

    #[test]
    fn aggregate_fail_escalates_exit_to_three_when_none_configured() {
        // Four non-Ok data sources -> the aggregate FAIL (Auth) is appended,
        // and worst_exit_code maps it to 3 even though no individual probe is
        // a Fail (they are all Warn).
        let ds = [
            CheckResult::warn(CheckCategory::Auth, "oauth not configured", None),
            CheckResult::warn(CheckCategory::Other, "no jsonl", None),
            CheckResult::warn(CheckCategory::Other, "no codex", None),
            CheckResult::warn(CheckCategory::Auth, "no key", None),
        ];
        assert!(no_data_source_configured(&ds.iter().collect::<Vec<_>>()));
        let mut results: Vec<CheckResult> = ds.to_vec();
        results.push(no_provider_aggregate_fail());
        assert_eq!(
            doctor_exit_code(&results, false),
            3,
            "aggregate FAIL must escalate a warn-only set to exit 3"
        );
    }

    #[test]
    fn aggregate_not_appended_when_one_data_source_ok() {
        // One Ok data source -> no aggregate appended; a lone WARN stays exit 0.
        let ds = [
            CheckResult::warn(CheckCategory::Auth, "oauth not configured", None),
            CheckResult::ok(CheckCategory::Other, "jsonl: 5 files"),
            CheckResult::warn(CheckCategory::Other, "no codex", None),
            CheckResult::warn(CheckCategory::Auth, "no key", None),
        ];
        assert!(!no_data_source_configured(&ds.iter().collect::<Vec<_>>()));
        // run_probes would NOT append the aggregate here, so the result set is
        // just the (warn-bearing) probes - exit stays 0 without --strict.
        let results: Vec<CheckResult> = ds.to_vec();
        assert_eq!(
            doctor_exit_code(&results, false),
            0,
            "one usable source must not be escalated by the aggregate"
        );
    }
}
