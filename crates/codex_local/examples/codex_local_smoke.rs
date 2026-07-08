//! Smoke test against the user's real `~/.codex/sessions/` data.
//!
//! Run with:
//!   cargo run --release -p codex_local --example codex_local_smoke
//!
//! Uses [`read_codex_quota`] - the exact entry point the app runs - so it
//! walks older sessions when the newest one hasn't logged a `token_count`
//! yet (fresh session / day rollover). Prints the latest snapshot the way
//! the tray / popover / CLI now classify it: both rolling windows labeled
//! by duration (5-hour and weekly), the worst window, plan type, and the
//! rate-limit-reached flag - plus the token/context/credits data that is
//! parsed internally but not yet surfaced in any UI (shown here so the
//! maintainer can eyeball it against the source files).
//!
//! Manual-test playbook for the maintainer:
//! 1. Run the example. Verify the session UUID matches a recent file under
//!    `~/.codex/sessions/`, and the plan type matches your ChatGPT plan.
//! 2. Verify each window's `used_percent` is plausible vs the Codex
//!    Analytics dashboard (chatgpt.com Codex usage view).
//! 3. Verify each reset countdown is in the future (a negative countdown
//!    means stale data or a system-clock issue).
//! 4. If you don't have Codex installed, the example exits cleanly with a
//!    "not installed" message and exit code 0.

use chrono::{DateTime, Utc};

use codex_local::{
    RateLimitWindow, WindowKind, find_codex_sessions_dir, find_latest_session, read_codex_quota,
};

/// Human label for a window, derived from its duration (never its slot).
fn window_label(w: &RateLimitWindow) -> &'static str {
    match w.kind() {
        WindowKind::FiveHour => "5-hour window",
        WindowKind::Weekly => "weekly window",
        WindowKind::Other => "window",
    }
}

fn print_window(w: &RateLimitWindow, now: DateTime<Utc>) {
    println!("{}:", window_label(w));
    println!("  Used:             {:.2}%", w.used_percent);
    println!(
        "  Window:           {} minutes ({:.1} days)",
        w.window_duration_minutes,
        w.window_duration_minutes as f64 / 1440.0
    );
    println!("  Resets at:        {}", w.resets_at.to_rfc3339());
    let until = w.resets_at - now;
    println!(
        "  Resets in:        {}h {}m",
        until.num_hours(),
        until.num_minutes() % 60
    );
}

fn main() -> anyhow::Result<()> {
    // read_codex_quota() is exactly what the app runs: it resolves the
    // sessions dir, then walks rollout files newest-first until one yields a
    // token_count snapshot (so a brand-new session with no quota event yet
    // doesn't mask yesterday's still-valid state).
    let snap = match read_codex_quota() {
        Ok(Some(s)) => s,
        Ok(None) => {
            // Installed, but no session carried a parseable token_count. Name
            // the newest file so the maintainer knows where to look.
            if let Ok(dir) = find_codex_sessions_dir() {
                println!("Scanned {}", dir.display());
                match find_latest_session(&dir)? {
                    Some(path) => println!(
                        "No quota data yet - newest session {} has no parseable token_count \
                         events, and no older session carried quota state either.",
                        path.display()
                    ),
                    None => println!("Codex installed but no rollout-*.jsonl files found yet."),
                }
            }
            return Ok(());
        }
        Err(codex_local::ParseError::FileMissing(p)) => {
            println!(
                "Codex CLI not installed (expected sessions dir: {})",
                p.display()
            );
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let now: DateTime<Utc> = Utc::now();

    println!();
    println!("=== Codex quota snapshot ===");
    println!("Observed at:        {}", snap.observed_at.to_rfc3339());
    println!("Session ID:         {}", snap.session_id);
    println!("Plan type:          {}", snap.plan_type);
    println!("Rate-limit reached: {}", snap.rate_limit_reached);
    if let Some(w) = snap.worst_window() {
        println!(
            "Worst window:       {} at {:.2}%",
            window_label(w),
            w.used_percent
        );
    }

    // Both windows, labeled by duration - matches how the tray, popover, and
    // CLI now classify them (never by primary/secondary slot, which varies by
    // plan). A single-window plan (e.g. "go") prints just the one it has.
    println!();
    for w in snap.windows() {
        print_window(w, now);
        println!();
    }

    // Internal, deferred data: parsed into #[serde(skip)] fields, not surfaced
    // in any UI yet. Printed here so it can be verified against the source.
    println!("--- internal (parsed, not shown in UI yet) ---");
    match &snap.tokens {
        Some(t) => {
            println!("Context window:     {} tokens", t.context_window);
            if t.context_window > 0 {
                println!(
                    "Context fill:       {:.1}% (last input {} / {})",
                    t.last_input_tokens as f64 / t.context_window as f64 * 100.0,
                    t.last_input_tokens,
                    t.context_window
                );
            }
            println!("Session tokens:     {}", t.session_total_tokens);
            match t.recent_burn_tokens_per_min {
                Some(b) => println!("Recent burn:        {b:.0} tokens/min"),
                None => println!("Recent burn:        n/a (<2 token_count samples this session)"),
            }
        }
        None => println!("Token/context:      none recorded in this event"),
    }
    match &snap.credits {
        Some(c) => println!(
            "Credits:            has_credits={}, balance={:?}",
            c.has_credits, c.balance
        ),
        None => println!("Credits:            none"),
    }

    Ok(())
}
