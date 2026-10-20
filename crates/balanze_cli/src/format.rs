//! Pure display/formatting helpers shared across the CLI renderers (compact,
//! sections, statusline). No I/O; integer money math stays i64 and only
//! crosses to f64 here at the display boundary (AGENTS.md §2.1).

use chrono::{DateTime, Duration, Utc};

/// Format an `i64` micro-USD value as a human-readable USD string. Delegates
/// to the `money` crate, which owns the one cent-rounding policy every Rust
/// and TypeScript surface shares - money never reaches f64 on this path.
pub(crate) fn micro_usd_to_display_dollars(micro: i64) -> String {
    money::micro_usd_to_display(micro)
}

/// Format an `i64` micro-USD value at the finer `$X.XXXX` grain, for the
/// per-line-item breakdowns whose rows are routinely sub-cent. Same integer
/// rounding rule as [`micro_usd_to_display_dollars`], finer grain.
pub(crate) fn micro_usd_to_display_precise(micro: i64) -> String {
    money::micro_usd_to_display_precise(micro)
}

/// Canonical Codex label, including an honest fallback for unknown durations.
pub(crate) fn format_codex_window(minutes: u64) -> String {
    codex_local::WindowKind::from_duration(minutes)
        .label()
        .to_string()
}

/// Format the age of a Codex snapshot for the rendered output. Returns
/// `None` for "fresh" snapshots (< 1 min old) so the common case stays
/// noise-free; otherwise returns a tight "Nm" / "Nh" / "Nd" tag.
///
/// Negative durations (observed_at in the future, e.g. clock skew) clamp
/// to `None` - we don't want to show "−2m old" or panic on subtraction.
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
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Preserve provider labels for cadence keys that have no known abbreviation.
pub(crate) fn cadence_label<'a>(key: &str, label: &'a str) -> &'a str {
    window::anthropic_label(key, label, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // -----------------------------------------------------------------------
    // format_codex_age - the freshness tag wired into compact + sections
    // views. The walker returns the newest-mtime rollout file regardless of
    // age (intentional, see crate doc); the renderer surfaces age so a 7d
    // stale snapshot can be distinguished from a 2-min-old one.
    // -----------------------------------------------------------------------

    fn t(year: i32, month: u32, day: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, h, m, 0).unwrap()
    }

    #[test]
    fn labels_match_shared_presentation_vectors() {
        let policy: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/presentation-policy.json"
        ))
        .unwrap();
        for case in policy["anthropicLabels"].as_array().unwrap() {
            let key = case["key"].as_str().unwrap();
            let source = case["supplied"].as_str().unwrap();
            assert_eq!(cadence_label(key, source), case["short"].as_str().unwrap());
            assert_eq!(
                window::anthropic_label(key, source, true),
                case["expanded"].as_str().unwrap()
            );
        }
        for case in policy["codexLabels"].as_array().unwrap() {
            assert_eq!(
                format_codex_window(case["minutes"].as_u64().unwrap()),
                case["label"].as_str().unwrap()
            );
        }
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
        // 5-hour Codex primary window (300 min) must read "5h", not "0d" -
        // 300 / 1440 floored to zero was the bug.
        assert_eq!(format_codex_window(300), "5h");
        // 7-day window (10080 min) reads "7d".
        assert_eq!(format_codex_window(10_080), "7d");
        // Exact-hour and sub-hour windows.
        assert_eq!(format_codex_window(60), "window");
        assert_eq!(format_codex_window(90), "window");
    }

    #[test]
    fn format_codex_window_uses_tolerant_known_labels() {
        assert_eq!(format_codex_window(295), "5h");
        assert_eq!(format_codex_window(305), "5h");
        assert_eq!(format_codex_window(10_075), "7d");
        assert_eq!(format_codex_window(10_085), "7d");
    }
}
