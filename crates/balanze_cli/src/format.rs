//! Pure display/formatting helpers shared across the CLI renderers (compact,
//! sections, statusline). No I/O; integer money math stays i64 and only
//! crosses to f64 here at the display boundary (AGENTS.md §2.1).

use chrono::{DateTime, Duration, Utc};

/// Format an `i64` micro-USD value as a human-readable USD string. Pure
/// display path per AGENTS.md §2.1: integer math everywhere internally;
/// f64 only at the boundary.
pub(crate) fn micro_usd_to_display_dollars(micro: i64) -> String {
    format!("${:.2}", micro as f64 / 1_000_000.0)
}

/// Render a Codex window duration in human units. Codex windows are commonly
/// 300 minutes (5h) or 10080 minutes (7d); dividing by 1440 and flooring
/// collapsed the 5h case to "0d". Pick the coarsest exact unit instead.
pub(crate) fn format_codex_window(minutes: u64) -> String {
    if minutes >= 1440 && minutes % 1440 == 0 {
        format!("{}d", minutes / 1440)
    } else if minutes >= 60 && minutes % 60 == 0 {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

/// Format the age of a Codex snapshot for the rendered output. Returns
/// `None` for "fresh" snapshots (< 1 min old) so the common case stays
/// noise-free; otherwise returns a tight "Nm" / "Nh" / "Nd" tag.
///
/// Negative durations (observed_at in the future, e.g. clock skew) clamp
/// to `None` — we don't want to show "−2m old" or panic on subtraction.
pub(crate) fn format_codex_age(
    observed_at: DateTime<Utc>,
    fetched_at: DateTime<Utc>,
) -> Option<String> {
    let age = fetched_at.signed_duration_since(observed_at);
    let total_secs = age.num_seconds();
    if total_secs < 60 {
        return None;
    }
    if total_secs < 3600 {
        return Some(format!("{}m", total_secs / 60));
    }
    if total_secs < 86_400 {
        return Some(format!("{}h", total_secs / 3600));
    }
    Some(format!("{}d", total_secs / 86_400))
}

/// Short cadence tag for the compact view, keyed off the **stable
/// cadence `key`** (e.g. "five_hour", "seven_day_sonnet") rather than
/// the free-form `display_label`. `anthropic_oauth` documents
/// `display_label` as curated-but-free-form, so matching on it is
/// fragile; the `key` is the wire-stable identifier.
///
/// Each 7-day sub-variant gets a distinct suffix so a user on a
/// Sonnet-only or Opus-only flow doesn't see two indistinguishable
/// "7d" cells (e.g. "19% 7d, 84% 7d-son"). Unknown / internal-codename
/// cadences render "?" here on purpose — the full label is visible in
/// `--sections`; the compact row is a glance, not the source of truth.
pub(crate) fn short_cadence(key: &str) -> &'static str {
    match key {
        "five_hour" => "5h",
        "seven_day" => "7d",
        "seven_day_sonnet" => "7d-son",
        "seven_day_opus" => "7d-opus",
        "seven_day_oauth_apps" => "7d-apps",
        "seven_day_cowork" => "7d-cowork",
        "seven_day_omelette" => "7d-omel",
        _ => "?",
    }
}

pub(crate) fn pretty_duration(d: Duration) -> String {
    if d.num_seconds() < 0 {
        return "(passed)".to_string();
    }
    let total_secs = d.num_seconds();
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let mins = (total_secs % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

pub(crate) fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // -----------------------------------------------------------------------
    // format_codex_age — the freshness tag wired into compact + sections
    // views. The walker returns the newest-mtime rollout file regardless of
    // age (intentional, see crate doc); the renderer surfaces age so a 7d
    // stale snapshot can be distinguished from a 2-min-old one.
    // -----------------------------------------------------------------------

    fn t(year: i32, month: u32, day: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, h, m, 0).unwrap()
    }

    #[test]
    fn format_codex_age_under_one_minute_is_silent() {
        let fetched = t(2026, 5, 20, 12, 0);
        let observed = fetched - Duration::seconds(30);
        assert_eq!(format_codex_age(observed, fetched), None);
    }

    #[test]
    fn format_codex_age_minutes_hours_days() {
        let fetched = t(2026, 5, 20, 12, 0);
        assert_eq!(
            format_codex_age(fetched - Duration::minutes(5), fetched).as_deref(),
            Some("5m")
        );
        assert_eq!(
            format_codex_age(fetched - Duration::hours(3), fetched).as_deref(),
            Some("3h")
        );
        assert_eq!(
            format_codex_age(fetched - Duration::days(8), fetched).as_deref(),
            Some("8d")
        );
    }

    #[test]
    fn format_codex_age_clamps_negative_to_none() {
        // observed_at AHEAD of fetched_at (clock skew / future-dated event).
        // Must not panic on subtraction and must not render "−5m old".
        let fetched = t(2026, 5, 20, 12, 0);
        let observed = fetched + Duration::minutes(5);
        assert_eq!(format_codex_age(observed, fetched), None);
    }

    #[test]
    fn format_codex_window_renders_human_units() {
        // 5-hour Codex primary window (300 min) must read "5h", not "0d" —
        // 300 / 1440 floored to zero was the bug.
        assert_eq!(format_codex_window(300), "5h");
        // 7-day window (10080 min) reads "7d".
        assert_eq!(format_codex_window(10_080), "7d");
        // Exact-hour and sub-hour windows.
        assert_eq!(format_codex_window(60), "1h");
        assert_eq!(format_codex_window(90), "90m");
    }
}
