#[cfg(test)]
mod tests {
    use super::select_anthropic_windows;

    #[test]
    fn selects_distinct_slots_and_keeps_first_duplicates_and_ties() {
        let rows = [
            ("five_hour", 20.0),
            ("five_hour", 90.0),
            ("seven_day", 60.0),
            ("seven_day_opus", 60.0),
            ("monthly_unknown", 80.0),
        ];
        let result = select_anthropic_windows(rows.iter().map(|w| (w, w.0, w.1)));
        assert_eq!(result.five_hour, Some(&rows[0]));
        assert_eq!(result.other, Some(&rows[4]));
        assert_eq!(result.weekly, Some(&rows[2]));
    }

    #[test]
    fn empty_and_missing_slots_remain_empty() {
        let rows = [("monthly_unknown", 80.0)];
        let result = select_anthropic_windows(rows.iter().map(|w| (w, w.0, w.1)));
        assert_eq!(result.five_hour, None);
        assert_eq!(result.other, Some(&rows[0]));
        assert_eq!(result.weekly, None);
        let empty = select_anthropic_windows(std::iter::empty::<(u8, &str, f32)>());
        assert_eq!(empty.five_hour, None);
        assert_eq!(empty.other, None);
        assert_eq!(empty.weekly, None);
    }
}
// Shared selection of Anthropic quota windows for compact surfaces.

/// Separate selections preserve the distinction between any non-five-hour
/// window and the strictly weekly window used by the tray preference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnthropicWindows<T> {
    pub five_hour: Option<T>,
    pub other: Option<T>,
    pub weekly: Option<T>,
}

/// Select by raw utilization. The first five-hour window and first equal
/// maximum win, retaining the identity and associated data of each window.
pub fn select_anthropic_windows<'a, T: Copy>(
    windows: impl IntoIterator<Item = (T, &'a str, f32)>,
) -> AnthropicWindows<T> {
    let mut five_hour = None;
    let mut other: Option<(T, f32)> = None;
    let mut weekly: Option<(T, f32)> = None;
    for (item, key, percent) in windows {
        if key == "five_hour" {
            five_hour.get_or_insert(item);
            continue;
        }
        if other.is_none_or(|(_, current)| percent > current) {
            other = Some((item, percent));
        }
        if (key == "seven_day" || key.starts_with("seven_day_"))
            && weekly.is_none_or(|(_, current)| percent > current)
        {
            weekly = Some((item, percent));
        }
    }
    AnthropicWindows {
        five_hour,
        other: other.map(|(item, _)| item),
        weekly: weekly.map(|(item, _)| item),
    }
}
