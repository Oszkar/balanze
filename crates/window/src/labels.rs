//! Canonical Anthropic quota labels shared by Rust presentation surfaces.

/// Label by stable cadence key. Unknown keys retain the provider's wording.
/// Expanded labels are used in cards and sections; compact labels in tight UI.
pub fn anthropic_label<'a>(key: &str, fallback: &'a str, expanded: bool) -> &'a str {
    let (short, long) = match key {
        "five_hour" => ("5h", "5-hour"),
        "seven_day" => ("7d", "7-day"),
        "seven_day_sonnet" => ("7d-son", "7-day Sonnet"),
        "seven_day_opus" => ("7d-opus", "7-day Opus"),
        "seven_day_oauth_apps" => ("7d-apps", "7-day OAuth apps"),
        "seven_day_cowork" => ("7d-cowork", "7-day Cowork"),
        "seven_day_omelette" => ("7d-omel", "7-day Omelette"),
        _ => return fallback,
    };
    if expanded { long } else { short }
}

#[cfg(test)]
mod tests {
    #[test]
    fn labels_depend_on_identity_and_preserve_unknowns() {
        assert_eq!(
            super::anthropic_label("five_hour", "Current 5-hour session", false),
            "5h"
        );
        assert_eq!(
            super::anthropic_label("five_hour", "5-hour", true),
            "5-hour"
        );
        assert_eq!(
            super::anthropic_label("seven_day_opus", "provider wording", false),
            "7d-opus"
        );
        assert_eq!(
            super::anthropic_label("monthly_phoenix", "Monthly Phoenix", true),
            "Monthly Phoenix"
        );
    }
}
