//! Smoke test against the real Anthropic OAuth usage endpoint.
//!
//! Reads `~/.claude/.credentials.json` (or `~/.config/claude/.credentials.json`),
//! calls `GET https://api.anthropic.com/api/oauth/usage`, prints a structured
//! summary. Token/credentials are NEVER printed.
//!
//! Run with:
//!   cargo run --release --example smoke -p anthropic_oauth

use anthropic_oauth::{fetch_usage, load, DEFAULT_API_BASE};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let creds = load()?;
    let oauth = creds.claude_ai_oauth;

    let now_ms = chrono::Utc::now().timestamp_millis();
    let mins_left = (oauth.expires_at - now_ms) / 60_000;
    println!(
        "Credentials loaded — subscription={:?} tier={:?} token expires in {} min",
        oauth.subscription_type, oauth.rate_limit_tier, mins_left
    );

    let client = reqwest::Client::builder()
        .user_agent("balanze/0.1.0-spike")
        .build()?;
    let snapshot = fetch_usage(
        &client,
        DEFAULT_API_BASE,
        &oauth.access_token,
        oauth.subscription_type,
        oauth.rate_limit_tier,
    )
    .await?;

    println!();
    println!("=== /api/oauth/usage response ===");
    println!("Org UUID: {:?}", snapshot.org_uuid);
    println!("Fetched at: {}", snapshot.fetched_at);
    println!();
    println!("Cadences ({}):", snapshot.cadences.len());
    for cadence in &snapshot.cadences {
        println!(
            "  {:32} {:>6.2}%   resets {}",
            cadence.display_label,
            cadence.utilization_percent,
            cadence.resets_at.to_rfc3339()
        );
    }
    if let Some(extra) = &snapshot.extra_usage {
        println!();
        let used = (extra.used_credits_micro_usd as f64) / 1_000_000.0;
        let limit = (extra.monthly_limit_micro_usd as f64) / 1_000_000.0;
        println!(
            "Extra usage: {} (enabled={}, {:.2} of {:.2} {} used, {:.1}%)",
            if extra.is_enabled {
                "ENABLED"
            } else {
                "disabled"
            },
            extra.is_enabled,
            used,
            limit,
            extra.currency,
            extra.utilization_percent
        );
    }
    Ok(())
}
