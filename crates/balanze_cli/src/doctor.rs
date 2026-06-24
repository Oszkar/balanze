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
use crate::probes::{self, CheckCategory, CheckLevel, CheckResult};

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

/// Run all six probes. `--offline` validates the OpenAI key for presence only;
/// otherwise it issues one fail-fast network request via the watcher.
fn run_probes(offline: bool) -> Vec<CheckResult> {
    let now = Utc::now();
    let mut results = Vec::with_capacity(6);
    results.push(probes::probe_claude_oauth(now));
    results.push(probes::probe_claude_jsonl());
    results.push(probes::probe_codex());

    let (key, presence) = probes::probe_openai_key_presence();
    match (offline, key) {
        (false, Some(key)) => {
            // A key resolved and we are online: one fail-fast request.
            match tokio::runtime::Runtime::new() {
                Ok(rt) => results.push(rt.block_on(probes::probe_openai_key_online(&key))),
                Err(e) => results.push(CheckResult::warn(
                    CheckCategory::Network,
                    format!("Could not start OpenAI validation runtime: {e}"),
                    Some("Re-run with --offline to skip network validation.".to_string()),
                )),
            }
        }
        // Offline, or no key to validate: the presence probe is the answer.
        _ => results.push(presence),
    }

    results.push(probes::probe_statusline());
    results.push(probes::probe_settings_and_keychain());
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
}
