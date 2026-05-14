//! Smoke test against the user's real `~/.codex/sessions/` data.
//!
//! Run with:
//!   cargo run --release -p codex_local --example codex_local_smoke
//!
//! Prints the latest rate-limit snapshot in the same shape the
//! eventual CLI / tray UI will surface: utilization %, window length,
//! reset countdown, plan type, rate-limit-reached flag, plus
//! provenance (which session file the snapshot came from).
//!
//! Manual-test playbook for the maintainer:
//! 1. Run the example. Verify the session file path matches a
//!    recent file under `~/.codex/sessions/`.
//! 2. Verify `used_percent` is plausible vs Codex CLI's own
//!    self-reporting (`codex usage` or equivalent — currently no
//!    user-facing CLI command for this, so eyeball it).
//! 3. Verify the reset countdown is in the future (negative
//!    countdown = stale data or system-clock issue).
//! 4. If you don't have Codex installed, the example exits cleanly
//!    with a "not installed" message and exit code 0.

use chrono::{DateTime, Utc};

use codex_local::{find_codex_sessions_dir, find_latest_session, read_latest_quota_snapshot};

fn main() -> anyhow::Result<()> {
    let dir = match find_codex_sessions_dir() {
        Ok(d) => d,
        Err(codex_local::ParseError::FileMissing(p)) => {
            println!(
                "Codex CLI not installed (expected sessions dir: {})",
                p.display()
            );
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    println!("Scanning {}", dir.display());

    let Some(path) = find_latest_session(&dir)? else {
        println!("Codex installed but no rollout-*.jsonl files found yet.");
        return Ok(());
    };
    println!("Latest session: {}", path.display());

    let snap = match read_latest_quota_snapshot(&path)? {
        Some(s) => s,
        None => {
            println!(
                "Latest session has zero parseable token_count events \
                 (probably crashed before quota accounting fired)."
            );
            return Ok(());
        }
    };

    println!();
    println!("=== Codex quota snapshot ===");
    println!("Observed at:        {}", snap.observed_at.to_rfc3339());
    println!("Session ID:         {}", snap.session_id);
    println!("Plan type:          {}", snap.plan_type);
    println!("Rate-limit reached: {}", snap.rate_limit_reached);

    println!();
    let now: DateTime<Utc> = Utc::now();
    println!("Primary window:");
    println!("  Used:             {:.2}%", snap.primary.used_percent);
    println!(
        "  Window:           {} minutes ({:.1} days)",
        snap.primary.window_duration_minutes,
        snap.primary.window_duration_minutes as f64 / 1440.0
    );
    println!(
        "  Resets at:        {}",
        snap.primary.resets_at.to_rfc3339()
    );
    let until = snap.primary.resets_at - now;
    println!(
        "  Resets in:        {}h {}m",
        until.num_hours(),
        until.num_minutes() % 60
    );

    if let Some(secondary) = &snap.secondary {
        println!();
        println!("Secondary window:");
        println!("  Used:             {:.2}%", secondary.used_percent);
        println!(
            "  Window:           {} minutes",
            secondary.window_duration_minutes
        );
        println!("  Resets at:        {}", secondary.resets_at.to_rfc3339());
    } else {
        println!();
        println!("Secondary window:   none (typical for non-enterprise plans)");
    }

    Ok(())
}
