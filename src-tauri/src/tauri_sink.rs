//! Production sink for the `state_coordinator` actor (AGENTS.md §4 #7 - the
//! only crate that may call Tauri tray APIs). Emits `usage_updated` /
//! `degraded_state` to the Svelte UI and enqueues gauge tray targets, deduped by
//! provider identity, fill, state, title, and tooltip per AGENTS.md §3.1.

use serde::Serialize;
use state_coordinator::{Sink, Snapshot, Source};
use tauri::{AppHandle, Emitter};

use crate::tray_icon;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorBucket {
    /// No quota signal from any source yet (cold start, or no provider
    /// configured). Painted grey by `bucket_for_view` so an empty state never
    /// reads as a healthy green.
    Neutral,
    Green,
    Yellow,
    Orange,
    Red,
    Warn,
}

impl From<window::Severity> for ColorBucket {
    fn from(sev: window::Severity) -> Self {
        match sev {
            window::Severity::Green => ColorBucket::Green,
            window::Severity::Yellow => ColorBucket::Yellow,
            window::Severity::Orange => ColorBucket::Orange,
            window::Severity::Red => ColorBucket::Red,
        }
    }
}

impl ColorBucket {
    /// Heat color for a utilization percentage, via the shared `window::Severity`
    /// classifier (50 / 75 / 90) - the same one the popover, CLI, and statusline
    /// use, so every surface agrees. `Neutral` (cold start) and `Warn` (a
    /// degraded source) are tray-only overlays applied in `bucket_for_view`.
    pub(crate) fn from_util(util_percent: f32) -> Self {
        window::Severity::from_util(util_percent).into()
    }
}

#[derive(Serialize, Clone)]
struct DegradedPayload {
    source: String,
    error: String,
}

fn source_key(source: Source) -> &'static str {
    match source {
        Source::ClaudeOAuth => "claude_oauth",
        Source::ClaudeJsonl => "claude_jsonl",
        Source::AnthropicApiCost => "anthropic_api_cost",
        Source::CodexQuota => "codex_quota",
        Source::OpenAiCosts => "openai_costs",
        Source::ClaudeStatusline => "claude_statusline",
    }
}

/// The tray's canonical windows, per provider: Claude 5h, Claude weekly (7d =
/// the worst of every non-5h cadence), Codex 5h, and Codex weekly (the worst of
/// every non-5h Codex window). The ring color, the menu-bar title, and the
/// tooltip ALL derive from this one view, so a red ring
/// always corresponds to a number the user can see. (The old bug: the ring came
/// from the worst window across everything, but the title only ever printed the
/// 5h - a red icon could sit next to "C 20%".) Folds the selected source's
/// windows. Source precedence comes from
/// `Snapshot::anthropic_quota_source` so tray, CLI, and TUI cannot disagree.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct TrayView<'a> {
    claude_5h: Option<f32>,
    claude_7d: Option<(&'a str, f32)>,
    claude_weekly: Option<(&'a str, f32)>,
    claude_stale: bool,
    codex_5h: Option<f32>,
    codex_weekly: Option<(&'static str, f32)>,
    codex_fixed_weekly: Option<(&'static str, f32)>,
    // Selected by codex_local before conversion to display precision. Detailed
    // slots must never re-select the Codex headline or its label.
    codex_headline: Option<(&'static str, f32)>,
    codex_expired: bool,
}

fn fold_max(slot: &mut Option<f32>, v: f32) {
    *slot = Some(slot.map_or(v, |cur| cur.max(v)));
}

// Keep the selected label and value inseparable; first wins equal percentages.
fn fold_labeled_max<'a>(slot: &mut Option<(&'a str, f32)>, value: (&'a str, f32)) {
    if slot.is_none_or(|(_, current)| value.1 > current) {
        *slot = Some(value);
    }
}

impl<'a> TrayView<'a> {
    fn from_snapshot(s: &'a Snapshot) -> Self {
        let mut v = TrayView::default();
        if let Some(view) = s.anthropic_presentation() {
            v.claude_stale = view.stale;
            v.claude_5h = view.windows.five_hour.map(|w| w.percent);
            let labeled = |w: state_coordinator::AnthropicPresentationWindow<'a>| {
                (window::anthropic_label(w.key, w.label, false), w.percent)
            };
            v.claude_7d = view.windows.other.map(labeled);
            v.claude_weekly = view.windows.weekly.map(labeled);
        }
        if let Some(q) = &s.codex_quota {
            // Keep the same last-known values every other surface shows, but
            // retain one stale bit so the tray uses its warning treatment rather
            // than presenting an expired rollout as confidently live.
            v.codex_expired = q.any_window_expired(s.fetched_at);
            v.codex_headline = q
                .worst_window()
                .map(|w| (w.kind().label(), w.used_percent as f32));
            for w in q
                .windows()
                .filter(|w| w.kind() == state_coordinator::WindowKind::FiveHour)
            {
                fold_max(&mut v.codex_5h, w.used_percent as f32);
            }
            v.codex_weekly = q
                .weekly_or_other()
                .map(|w| (w.kind().label(), w.used_percent as f32));
            for w in q
                .windows()
                .filter(|w| w.kind() == state_coordinator::WindowKind::Weekly)
            {
                fold_labeled_max(
                    &mut v.codex_fixed_weekly,
                    (w.kind().label(), w.used_percent as f32),
                );
            }
        }
        v
    }

    /// Any live quota signal at all - distinguishes real data (paint a heat
    /// color) from a cold-start snapshot (paint Neutral).
    fn has_data(&self) -> bool {
        self.claude_5h.is_some()
            || self.claude_7d.is_some()
            || self.codex_5h.is_some()
            || self.codex_weekly.is_some()
    }

    /// Claude's worst window (max of 5h and 7d) - the single Claude figure shown
    /// in the menu-bar title.
    #[cfg(test)]
    fn claude_worst(&self) -> Option<f32> {
        match (self.claude_5h, self.claude_7d.map(|(_, pct)| pct)) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// The single worst window across providers, labeled - drives the ring color
    /// and the tooltip header. Compared on the raw value; callers round for
    /// display. Codex contributes its canonical headline, preserving its window
    /// identity on ties. Cross-provider ties retain the existing Codex priority.
    fn worst(&self) -> Option<(String, f32)> {
        [
            self.claude_5h.map(|p| ("Claude 5h".to_string(), p)),
            self.claude_7d
                .map(|(label, p)| (format!("Claude {label}"), p)),
            self.codex_headline
                .map(|(label, p)| (format!("Codex {label}"), p)),
        ]
        .into_iter()
        .flatten()
        .max_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Rounded utilization of the worst window; the ring classifies THIS so its
    /// color matches the largest number the title/tooltip display (89.6 shows
    /// "90%" and must read Red, not Orange).
    #[cfg(test)]
    fn worst_rounded(&self) -> f32 {
        self.worst().map_or(0.0, |(_, p)| p.round())
    }

    fn selected(&self, choice: settings::TrayWindow) -> Option<(String, f32)> {
        match choice {
            settings::TrayWindow::Highest => self.worst(),
            settings::TrayWindow::FiveHour => [
                self.claude_5h.map(|p| ("Claude 5h".to_string(), p)),
                self.codex_5h.map(|p| ("Codex 5h".to_string(), p)),
            ]
            .into_iter()
            .flatten()
            .max_by(|a, b| a.1.total_cmp(&b.1)),
            settings::TrayWindow::Weekly => [
                self.claude_weekly
                    .map(|(label, p)| (format!("Claude {label}"), p)),
                self.codex_fixed_weekly
                    .map(|(label, p)| (format!("Codex {label}"), p)),
            ]
            .into_iter()
            .flatten()
            .max_by(|a, b| a.1.total_cmp(&b.1)),
        }
    }
}

/// Pick the tray color for a view. A degraded source forces the warning bucket;
/// a view with no quota signal yet is Neutral; otherwise the worst window's
/// rounded utilization maps to a heat color.
#[cfg(test)]
fn bucket_for_view(view: &TrayView, degraded: bool) -> ColorBucket {
    if degraded {
        ColorBucket::Warn
    } else if !view.has_data() {
        ColorBucket::Neutral
    } else {
        ColorBucket::from_util(view.worst_rounded())
    }
}

/// Menu-bar title (macOS) / tooltip line 1: `Claude X% · Codex Y%`, both
/// providers in fixed slots, each showing that provider's worst window. The
/// fill color matches the larger of the two shown numbers. An unconfigured provider
/// is omitted.
#[cfg(test)]
fn tray_title(view: &TrayView) -> String {
    let c = view
        .claude_worst()
        .map(|p| format!("Claude {}%", p.round() as i64));
    let o = view
        .codex_headline
        .map(|(_, p)| format!("Codex {}%", p.round() as i64));
    [c, o].into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

/// Tooltip: a fixed-layout status panel. A header names the worst window so the
/// fill color is explained, then both providers in stable slots. Kept compact
/// for the Windows ~128-char tooltip cap - per-window reset times live in the
/// popover, not here.
fn tray_tooltip(view: &TrayView, degraded: bool) -> String {
    if !view.has_data() {
        return if degraded {
            "Balanze - quota unavailable".to_string()
        } else {
            "Balanze - connecting...".to_string()
        };
    }
    let mut lines: Vec<String> = Vec::new();
    if let Some((label, pct)) = view.worst() {
        lines.push(format!("Balanze - worst: {label} {}%", pct.round() as i64));
    }
    let mut claude = Vec::new();
    if let Some(p) = view.claude_5h {
        claude.push(format!("5h {}%", p.round() as i64));
    }
    if let Some((label, p)) = view.claude_7d {
        claude.push(format!("{label} {}%", p.round() as i64));
    }
    if !claude.is_empty() {
        lines.push(format!("Claude  {}", claude.join("  ")));
    }
    let mut codex = Vec::new();
    if let Some(p) = view.codex_5h {
        codex.push(format!("5h {}%", p.round() as i64));
    }
    if let Some((label, p)) = view.codex_weekly {
        codex.push(format!("{label} {}%", p.round() as i64));
    }
    if !codex.is_empty() {
        lines.push(format!("Codex   {}", codex.join("  ")));
    }
    if degraded {
        lines.push("(some data may be stale)".to_string());
    }
    lines.join("\n")
}

/// Build presentation targets only from coordinator-owned inputs.
fn paint_targets(snapshot: &Snapshot, settings: &settings::Settings) -> tray_icon::PaintSet {
    use tray_icon::{IconState, Provider};
    let view = TrayView::from_snapshot(snapshot);
    let claude = TrayView {
        codex_5h: None,
        codex_weekly: None,
        codex_fixed_weekly: None,
        codex_headline: None,
        codex_expired: false,
        ..view
    };
    let codex = TrayView {
        claude_5h: None,
        claude_7d: None,
        claude_weekly: None,
        claude_stale: false,
        ..view
    };
    let choice = settings.tray.window;
    let claude_state = if view.claude_stale {
        IconState::Stale
    } else if claude.has_data() && claude.selected(choice).is_none() {
        IconState::Missing
    } else if claude.has_data() {
        IconState::Fresh
    } else if snapshot.claude_oauth_error.is_some() || snapshot.claude_statusline_error.is_some() {
        IconState::Error
    } else if snapshot.claude_oauth_unavailable.is_some() || !settings.providers.anthropic_enabled {
        IconState::Missing
    } else {
        IconState::Connecting
    };
    let codex_state = if snapshot.codex_quota_error.is_some() {
        if codex.has_data() {
            IconState::Stale
        } else {
            IconState::Error
        }
    } else if view.codex_expired {
        IconState::Stale
    } else if codex.has_data() && codex.selected(choice).is_none() {
        IconState::Missing
    } else if codex.has_data() {
        IconState::Fresh
    } else {
        IconState::Missing
    };
    let make = |provider: Provider, provider_view: &TrayView<'_>, state: IconState| {
        let selected = provider_view.selected(choice);
        let percent = selected.as_ref().map(|(_, p)| p.round());
        let title = if settings.tray.show_percentages && percent.is_some() {
            format!(
                "{} {}%",
                provider.short_name(),
                percent.unwrap_or_default() as i64
            )
        } else {
            provider.short_name().to_string()
        };
        let title = if state.degraded() {
            format!("{title} !")
        } else {
            title
        };
        let detail = selected.map_or_else(
            || {
                if provider_view.has_data() {
                    let unavailable = format!(
                        "{} window unavailable",
                        match choice {
                            settings::TrayWindow::FiveHour => "5-hour",
                            settings::TrayWindow::Weekly => "Weekly",
                            settings::TrayWindow::Highest => "Quota",
                        }
                    );
                    if state.degraded() {
                        format!("{unavailable}\n{}", state.description())
                    } else {
                        unavailable
                    }
                } else {
                    state.description().to_string()
                }
            },
            |(window, p)| {
                format!(
                    "{window} {}% used\n{}",
                    p.round() as i64,
                    state.description()
                )
            },
        );
        tray_icon::PaintTarget {
            icon: tray_icon::IconTarget {
                provider,
                bucket: if state.degraded() {
                    ColorBucket::Warn
                } else {
                    percent.map_or(ColorBucket::Neutral, ColorBucket::from_util)
                },
                fill: percent.map(|p| p.clamp(0.0, 100.0) as u8),
                state,
            },
            title,
            tooltip: format!("Balanze - {}\n{detail}", provider.full_name()),
        }
    };
    let mut c = make(Provider::Claude, &claude, claude_state);
    let o = make(Provider::Codex, &codex, codex_state);
    if settings.tray.mode == settings::TrayMode::Separate {
        return tray_icon::PaintSet {
            main: c,
            codex: settings.providers.codex_enabled.then_some(o),
        };
    }
    // Preserve Codex priority on ties, and select using raw values before rounding.
    let codex_wins = settings.providers.codex_enabled
        && codex
            .selected(choice)
            .is_some_and(|(_, p)| claude.selected(choice).is_none_or(|(_, c)| p >= c));
    if codex_wins {
        c = o;
    }
    let degraded =
        claude_state.degraded() || (settings.providers.codex_enabled && codex_state.degraded());
    if degraded && !c.icon.state.degraded() {
        // The selected value is current, but the combined indicator must also
        // disclose an unavailable/stale participating provider.
        c.icon.state = IconState::Partial;
        c.icon.bucket = ColorBucket::Warn;
        c.title.push_str(" !");
    }
    let combined_view = if settings.providers.codex_enabled {
        view
    } else {
        claude
    };
    c.tooltip = if choice == settings::TrayWindow::Highest {
        tray_tooltip(&combined_view, degraded)
    } else {
        let describe = |name: &str, provider: &TrayView<'_>| {
            provider.selected(choice).map_or_else(
                || format!("{name}: selected window unavailable"),
                |(label, pct)| format!("{label}: {}%", pct.round() as i64),
            )
        };
        let mut lines = vec![describe("Claude", &claude)];
        if settings.providers.codex_enabled {
            lines.push(describe("Codex", &codex));
        }
        if degraded {
            lines.push("Some data may be stale".to_string());
        }
        format!("Balanze\n{}", lines.join("\n"))
    };
    tray_icon::PaintSet {
        main: c,
        codex: None,
    }
}

pub(crate) struct TauriSink {
    app: AppHandle,
    repaint: tokio::sync::watch::Sender<Option<tray_icon::PaintSet>>,
    last_requested: Option<tray_icon::PaintSet>,
    settings: settings::Settings,
}

impl TauriSink {
    pub(crate) fn new(app: AppHandle) -> (Self, tokio::task::JoinHandle<Result<(), String>>) {
        let (repaint, painter) = tray_icon::spawn_painter(app.clone());
        (
            Self {
                app,
                repaint,
                last_requested: None,
                settings: settings::Settings::default(),
            },
            painter,
        )
    }

    fn enqueue(&mut self, snapshot: &Snapshot) {
        let target = paint_targets(snapshot, &self.settings);
        if self.last_requested.as_ref() != Some(&target) {
            self.repaint.send_replace(Some(target.clone()));
            self.last_requested = Some(target);
        }
    }
}

impl Sink for TauriSink {
    fn on_settings(&mut self, settings: &settings::Settings) {
        self.settings = settings.clone();
    }

    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        if let Err(e) = self.app.emit("usage_updated", snapshot) {
            tracing::warn!("tauri_sink: emit usage_updated failed: {e}");
        }
        self.enqueue(snapshot);
    }

    fn on_snapshot_durable(&mut self, snapshot: &Snapshot) {
        // Error-only updates carry the canonical error slots and fallback
        // selection. Repaint them without duplicating usage_updated events.
        self.enqueue(snapshot);
    }

    fn on_degraded(&mut self, source: Source, error: &str) {
        let payload = DegradedPayload {
            source: source_key(source).to_string(),
            error: error.to_string(),
        };
        if let Err(e) = self.app.emit("degraded_state", payload) {
            tracing::warn!("tauri_sink: emit degraded_state failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test-only convenience wrappers over the TrayView API. The prod paint path
    // builds the view once in `paint_target`; these keep the coloring/quota
    // tests reading against `&Snapshot`.
    fn worst_utilization(s: &Snapshot) -> f32 {
        TrayView::from_snapshot(s).worst().map_or(0.0, |(_, p)| p)
    }
    fn has_quota_data(s: &Snapshot) -> bool {
        TrayView::from_snapshot(s).has_data()
    }
    fn select_bucket(s: &Snapshot, degraded: bool) -> ColorBucket {
        bucket_for_view(&TrayView::from_snapshot(s), degraded)
    }

    #[test]
    fn tooltip_preserves_variant_and_unknown_labels() {
        let mut snapshot = Snapshot::empty(chrono::Utc::now());
        let mut oauth = oauth_5h_7d(10.0, 91.0);
        oauth.cadences[1].key = "seven_day_opus".into();
        oauth.cadences[1].display_label = "Provider Opus wording".into();
        snapshot.claude_oauth = Some(oauth);
        let mut codex = codex_5h_weekly(20.0, 95.0);
        codex.secondary.as_mut().unwrap().window_duration_minutes = 4320;
        snapshot.codex_quota = Some(codex);
        let view = TrayView::from_snapshot(&snapshot);
        let tooltip = tray_tooltip(&view, false);
        assert!(tooltip.contains("worst: Codex window 95%"), "{tooltip}");
        assert!(tooltip.contains("7d-opus 91%"), "{tooltip}");
        assert!(tooltip.contains("window 95%"), "{tooltip}");
        assert!(!tooltip.contains("7d 95%"), "{tooltip}");
        let cadence = &mut snapshot.claude_oauth.as_mut().unwrap().cadences[1];
        cadence.key = "monthly_phoenix".into();
        cadence.display_label = "Monthly Phoenix".into();
        let tooltip = tray_tooltip(&TrayView::from_snapshot(&snapshot), false);
        assert!(tooltip.contains("Monthly Phoenix 91%"), "{tooltip}");
    }

    fn fresh_oauth(five: f32, seven: f32) -> anthropic_oauth::ClaudeOAuthSnapshot {
        let mut oauth = oauth_5h_7d(five, seven);
        for cadence in &mut oauth.cadences {
            cadence.resets_at = oauth.fetched_at + chrono::Duration::hours(1);
        }
        oauth
    }

    #[test]
    fn shared_alert_scenarios_render_tray_targets() {
        for line in include_str!("../../tests/fixtures/alert-parity.csv")
            .lines()
            .skip(1)
        {
            let fields: Vec<_> = line.split(',').collect();
            let percent: f32 = fields[1].parse().unwrap();
            let stale: bool = fields[2].parse().unwrap();
            let mut snapshot = Snapshot::empty(chrono::Utc::now());
            snapshot.claude_oauth = Some(fresh_oauth(percent, 0.0));
            snapshot.fetched_at = snapshot.claude_oauth.as_ref().unwrap().fetched_at;
            if stale {
                snapshot.claude_oauth_error = Some("failed".into());
            }
            let target = paint_targets(&snapshot, &settings::Settings::default()).main;
            assert_eq!(
                target.icon.fill,
                Some(percent.round() as u8),
                "{}",
                fields[0]
            );
            assert_eq!(
                target.icon.bucket,
                if stale {
                    ColorBucket::Warn
                } else {
                    match fields[5] {
                        "green" => ColorBucket::Green,
                        "yellow" => ColorBucket::Yellow,
                        "orange" => ColorBucket::Orange,
                        "red" => ColorBucket::Red,
                        _ => panic!("unknown severity"),
                    }
                },
                "{}",
                fields[0]
            );
            assert_eq!(target.icon.state.degraded(), stale, "{}", fields[0]);
        }
    }

    #[test]
    fn tray_window_preference_selects_named_window_and_reports_missing() {
        let mut s = Snapshot::empty(chrono::Utc::now());
        s.claude_oauth = Some(fresh_oauth(20.0, 95.0));
        s.codex_quota = Some(codex_5h_weekly(65.0, 80.0));
        s.fetched_at = chrono::Utc::now();
        let mut settings = settings::Settings::default();
        assert_eq!(paint_targets(&s, &settings).main.icon.fill, Some(95));
        settings.tray.window = settings::TrayWindow::FiveHour;
        let separate = paint_targets(&s, &settings);
        assert_eq!(separate.main.icon.fill, Some(20));
        assert_eq!(separate.codex.unwrap().icon.fill, Some(65));
        settings.tray.mode = settings::TrayMode::Combined;
        let combined = paint_targets(&s, &settings);
        assert_eq!(combined.main.icon.fill, Some(65));
        assert!(combined.main.tooltip.contains("Codex 5h: 65%"));
        settings.tray.window = settings::TrayWindow::Weekly;
        assert_eq!(paint_targets(&s, &settings).main.icon.fill, Some(95));
        s.codex_quota
            .as_mut()
            .unwrap()
            .secondary
            .as_mut()
            .unwrap()
            .window_duration_minutes = 4320;
        let combined = paint_targets(&s, &settings);
        assert!(
            combined
                .main
                .tooltip
                .contains("Codex: selected window unavailable")
        );
        settings.tray.mode = settings::TrayMode::Separate;
        let codex = paint_targets(&s, &settings).codex.unwrap();
        assert_eq!(codex.icon.fill, None);
        assert!(codex.tooltip.contains("Weekly window unavailable"));
        s.codex_quota_error = Some("read failed".into());
        let codex = paint_targets(&s, &settings).codex.unwrap();
        assert_eq!(codex.icon.bucket, ColorBucket::Warn);
        assert!(codex.tooltip.contains("stale"));
    }

    #[test]
    fn billing_and_activity_errors_do_not_change_quota_targets() {
        let mut s = Snapshot::empty(chrono::Utc::now());
        s.claude_oauth = Some(fresh_oauth(32.0, 78.0));
        s.fetched_at = chrono::Utc::now();
        let settings = settings::Settings::default();
        let before = paint_targets(&s, &settings);
        s.openai_error = Some("billing failed".into());
        s.claude_jsonl_error = Some("activity failed".into());
        assert_eq!(before, paint_targets(&s, &settings));
    }

    #[test]
    fn anthropic_slots_keep_first_five_and_distinguish_weekly_from_unknown() {
        let mut snapshot = Snapshot::empty(chrono::Utc::now());
        let mut oauth = fresh_oauth(20.0, 60.0);
        let mut duplicate = oauth.cadences[0].clone();
        duplicate.utilization_percent = 90.0;
        oauth.cadences.push(duplicate);
        let mut unknown = oauth.cadences[1].clone();
        unknown.key = "monthly_unknown".into();
        unknown.display_label = "Monthly".into();
        unknown.utilization_percent = 80.0;
        oauth.cadences.push(unknown);
        snapshot.fetched_at = oauth.fetched_at;
        snapshot.claude_oauth = Some(oauth);
        let view = TrayView::from_snapshot(&snapshot);
        assert_eq!(view.claude_5h, Some(20.0));
        assert_eq!(view.claude_7d, Some(("Monthly", 80.0)));
        assert_eq!(view.claude_weekly, Some(("7d", 60.0)));
    }

    #[test]
    fn tray_matches_cross_surface_rounded_threshold_table() {
        let cases = [
            (49.4, ColorBucket::Green),
            (49.5, ColorBucket::Yellow),
            (74.4, ColorBucket::Yellow),
            (74.5, ColorBucket::Orange),
            (89.4, ColorBucket::Orange),
            (89.5, ColorBucket::Red),
            (125.0, ColorBucket::Red),
        ];

        for (util, expected) in cases {
            let view = TrayView {
                claude_5h: Some(util),
                ..TrayView::default()
            };
            assert_eq!(bucket_for_view(&view, false), expected, "util={util}");
        }
    }

    /// Build a Claude OAuth snapshot carrying a single 5-hour cadence at the
    /// given utilization, shared by the worst-util and bucket-selection tests.
    fn oauth_with_util(util: f32) -> anthropic_oauth::ClaudeOAuthSnapshot {
        use chrono::Utc;
        anthropic_oauth::ClaudeOAuthSnapshot {
            cadences: vec![anthropic_oauth::CadenceBar {
                key: "five_hour".into(),
                display_label: "5h".into(),
                utilization_percent: util,
                resets_at: Utc::now(),
            }],
            extra_usage: None,
            subscription_type: None,
            rate_limit_tier: None,
            org_uuid: None,
            fetched_at: Utc::now(),
        }
    }

    #[test]
    fn worst_util_picks_max_across_sources() {
        use chrono::Utc;
        let mut s = Snapshot::empty(Utc::now());
        assert_eq!(worst_utilization(&s), 0.0);
        s.claude_oauth = Some(oauth_with_util(62.0));
        assert_eq!(worst_utilization(&s), 62.0);
    }

    /// A statusline window beyond five_hour/seven_day (e.g. a per-model
    /// weekly bucket) must still drive the tray color - the OAuth cadence
    /// loop above already considers every cadence generically; the
    /// statusline branch must match, not stay capped at the two named
    /// windows. Regression for a real gap: a critical Fable-style window at
    /// 95% while the named windows are low previously left the tray green.
    #[test]
    fn statusline_window_beyond_named_ones_drives_worst_utilization() {
        use chrono::Utc;
        let now = Utc::now();
        let mut s = Snapshot::empty(now);
        s.claude_statusline = Some(claude_statusline::StatuslineFilePayload::new(
            claude_statusline::StatuslineSnapshot {
                rate_limits: Some(claude_statusline::RateLimits {
                    windows: vec![
                        claude_statusline::RateWindow {
                            key: "five_hour".to_string(),
                            label: "5-hour".to_string(),
                            used_percent: 10.0,
                            resets_at: Utc::now(),
                        },
                        claude_statusline::RateWindow {
                            key: "seven_day_fable".to_string(),
                            label: "Seven Day Fable".to_string(),
                            used_percent: 95.0,
                            resets_at: Utc::now(),
                        },
                    ],
                }),
                session_cost_micro_usd: None,
                claude_code_version: None,
                model_display_name: None,
                context_used_percent: None,
            },
            now,
        ));
        assert_eq!(worst_utilization(&s), 95.0);
        assert!(has_quota_data(&s));
    }

    #[test]
    fn fresh_statusline_replaces_oauth_instead_of_folding_both() {
        use chrono::Utc;
        let now = Utc::now();
        let mut s = Snapshot::empty(now);
        s.claude_oauth = Some(oauth_with_util(95.0));
        s.claude_statusline = Some(claude_statusline::StatuslineFilePayload::new(
            claude_statusline::StatuslineSnapshot {
                rate_limits: Some(claude_statusline::RateLimits {
                    windows: vec![claude_statusline::RateWindow {
                        key: "five_hour".to_string(),
                        label: "5-hour".to_string(),
                        used_percent: 10.0,
                        resets_at: now,
                    }],
                }),
                session_cost_micro_usd: None,
                claude_code_version: None,
                model_display_name: None,
                context_used_percent: None,
            },
            now,
        ));

        assert_eq!(worst_utilization(&s), 10.0);
        s.claude_oauth_error = Some("OAuth failed while statusline is current".into());
        let target = paint_targets(&s, &settings::Settings::default());
        assert_eq!(target.main.icon.state, tray_icon::IconState::Fresh);
        assert_eq!(target.main.icon.bucket, ColorBucket::Green);
        assert_eq!(target.main.icon.fill, Some(10));
    }

    /// A stale statusLine payload (frozen file - another tool owns the slot)
    /// retains the last-known number with a warning, even when no source error
    /// was recorded. It must not paint the normal utilization heat color.
    #[test]
    fn stale_statusline_is_retained_with_warning() {
        use chrono::{Duration, Utc};
        let now = Utc::now();
        let mut s = Snapshot::empty(now);
        s.claude_statusline = Some(claude_statusline::StatuslineFilePayload::new(
            claude_statusline::StatuslineSnapshot {
                rate_limits: Some(claude_statusline::RateLimits {
                    windows: vec![claude_statusline::RateWindow {
                        key: "five_hour".to_string(),
                        label: "5-hour".to_string(),
                        used_percent: 95.0,
                        resets_at: now,
                    }],
                }),
                session_cost_micro_usd: None,
                claude_code_version: None,
                model_display_name: None,
                context_used_percent: None,
            },
            // Captured 100h before this snapshot's fetched_at -> stale.
            now - Duration::hours(100),
        ));
        let view = TrayView::from_snapshot(&s);
        assert_eq!(view.claude_5h, Some(95.0));
        assert!(view.claude_stale);
        assert_eq!(bucket_for_view(&view, view.claude_stale), ColorBucket::Warn);
        assert!(tray_tooltip(&view, view.claude_stale).contains("stale"));
    }

    /// A future-dated payload (captured_at ahead of fetched_at - the clock moved
    /// backward after the write) is equally untrusted: an upper-bound-only check
    /// would treat it as fresh and let it heat the tray.
    #[test]
    fn future_dated_statusline_is_retained_with_warning() {
        use chrono::{Duration, Utc};
        let now = Utc::now();
        let mut s = Snapshot::empty(now);
        s.claude_statusline = Some(claude_statusline::StatuslineFilePayload::new(
            claude_statusline::StatuslineSnapshot {
                rate_limits: Some(claude_statusline::RateLimits {
                    windows: vec![claude_statusline::RateWindow {
                        key: "five_hour".to_string(),
                        label: "5-hour".to_string(),
                        used_percent: 95.0,
                        resets_at: now,
                    }],
                }),
                session_cost_micro_usd: None,
                claude_code_version: None,
                model_display_name: None,
                context_used_percent: None,
            },
            // Captured 100h AFTER this snapshot's fetched_at -> negative age.
            now + Duration::hours(100),
        ));
        let view = TrayView::from_snapshot(&s);
        assert_eq!(view.claude_5h, Some(95.0));
        assert!(view.claude_stale);
        assert_eq!(bucket_for_view(&view, view.claude_stale), ColorBucket::Warn);
    }

    #[test]
    fn empty_snapshot_has_no_quota_data() {
        use chrono::Utc;
        assert!(!has_quota_data(&Snapshot::empty(Utc::now())));
    }

    #[test]
    fn oauth_cadence_counts_as_quota_data() {
        use chrono::Utc;
        let mut s = Snapshot::empty(Utc::now());
        s.claude_oauth = Some(oauth_with_util(0.0));
        assert!(has_quota_data(&s));
    }

    #[test]
    fn empty_snapshot_paints_neutral_not_green() {
        use chrono::Utc;
        // The fix: a cold-start snapshot must read as Neutral, never the healthy
        // Green that worst_utilization's 0.0 would otherwise produce.
        assert_eq!(
            select_bucket(&Snapshot::empty(Utc::now()), false),
            ColorBucket::Neutral
        );
    }

    #[test]
    fn populated_zero_util_is_green_not_neutral() {
        use chrono::Utc;
        // Real data at 0% is healthy Green - distinct from "no data" Neutral.
        let mut s = Snapshot::empty(Utc::now());
        s.claude_oauth = Some(oauth_with_util(0.0));
        assert_eq!(select_bucket(&s, false), ColorBucket::Green);
    }

    #[test]
    fn degraded_overrides_neutral() {
        use chrono::Utc;
        // A failing source paints the warning bucket even with no quota data.
        assert_eq!(
            select_bucket(&Snapshot::empty(Utc::now()), true),
            ColorBucket::Warn
        );
    }

    // --- tray title + tooltip (PR2) ---

    fn oauth_5h_7d(five: f32, seven: f32) -> anthropic_oauth::ClaudeOAuthSnapshot {
        use chrono::Utc;
        anthropic_oauth::ClaudeOAuthSnapshot {
            cadences: vec![
                anthropic_oauth::CadenceBar {
                    key: "five_hour".into(),
                    display_label: "5h".into(),
                    utilization_percent: five,
                    resets_at: Utc::now(),
                },
                anthropic_oauth::CadenceBar {
                    key: "seven_day".into(),
                    display_label: "7d".into(),
                    utilization_percent: seven,
                    resets_at: Utc::now(),
                },
            ],
            extra_usage: None,
            subscription_type: None,
            rate_limit_tier: None,
            org_uuid: None,
            fetched_at: Utc::now(),
        }
    }

    /// Codex windows are LIVE by default: `resets_at` is explicitly in the
    /// future rather than `Utc::now()`, so a fixture cannot land on the wrong
    /// side of `RateLimitWindow::expired` by evaluation-order luck.
    fn codex_with_util(util: f64) -> codex_local::CodexQuotaSnapshot {
        use chrono::{Duration, Utc};
        codex_local::CodexQuotaSnapshot {
            observed_at: Utc::now(),
            session_id: "test".into(),
            primary: codex_local::RateLimitWindow {
                used_percent: util,
                window_duration_minutes: 10080,
                resets_at: Utc::now() + Duration::hours(1),
            },
            secondary: None,
            plan_type: "go".into(),
            rate_limit_reached: false,
            tokens: None,
            credits: None,
        }
    }

    fn codex_5h_weekly(five: f64, weekly: f64) -> codex_local::CodexQuotaSnapshot {
        use chrono::{Duration, Utc};
        codex_local::CodexQuotaSnapshot {
            observed_at: Utc::now(),
            session_id: "test".into(),
            primary: codex_local::RateLimitWindow {
                used_percent: five,
                window_duration_minutes: 300,
                resets_at: Utc::now() + Duration::hours(1),
            },
            secondary: Some(codex_local::RateLimitWindow {
                used_percent: weekly,
                window_duration_minutes: 10080,
                resets_at: Utc::now() + Duration::days(3),
            }),
            plan_type: "pro".into(),
            rate_limit_reached: false,
            tokens: None,
            credits: None,
        }
    }

    /// Expired Codex values stay visible, matching every detailed surface, but
    /// the tray carries the rollout-level stale bit and paints Warning rather
    /// than presenting the old percentage as confidently live.
    #[test]
    fn expired_codex_window_stays_visible_with_stale_warning() {
        use chrono::{Duration, Utc};
        let now = Utc::now();
        let mut s = Snapshot::empty(now);
        let mut q = codex_5h_weekly(85.0, 4.0);
        q.primary.resets_at = now - Duration::hours(2); // 5h window long since reset
        s.codex_quota = Some(q);

        let view = TrayView::from_snapshot(&s);
        assert_eq!(view.codex_5h, Some(85.0));
        assert_eq!(view.codex_weekly, Some(("7d", 4.0)));
        assert!(view.codex_expired);
        assert_eq!(tray_title(&view), "Codex 85%");
        assert_eq!(
            bucket_for_view(&view, view.codex_expired),
            ColorBucket::Warn
        );
        assert!(tray_tooltip(&view, view.codex_expired).contains("stale"));
    }

    /// Even when every window expired, last-known numbers remain available and
    /// are explicitly stale rather than silently disappearing.
    #[test]
    fn fully_expired_codex_remains_visible_and_warns() {
        use chrono::{Duration, Utc};
        let now = Utc::now();
        let mut s = Snapshot::empty(now);
        let mut q = codex_5h_weekly(85.0, 92.0);
        q.primary.resets_at = now - Duration::hours(2);
        q.secondary.as_mut().unwrap().resets_at = now - Duration::hours(1);
        s.codex_quota = Some(q);

        let view = TrayView::from_snapshot(&s);
        assert!(view.has_data(), "last-known values remain visible");
        assert_eq!(tray_title(&view), "Codex 92%");
        assert_eq!(
            bucket_for_view(&view, view.codex_expired),
            ColorBucket::Warn
        );
    }

    #[test]
    fn codex_splits_into_5h_and_weekly() {
        use chrono::Utc;
        let mut s = Snapshot::empty(Utc::now());
        s.codex_quota = Some(codex_5h_weekly(12.0, 80.0));
        let view = TrayView::from_snapshot(&s);
        assert_eq!(view.codex_5h, Some(12.0));
        assert_eq!(view.codex_weekly, Some(("7d", 80.0)));
        assert_eq!(tray_title(&view), "Codex 80%");
        let tip = tray_tooltip(&view, false);
        assert!(tip.contains("worst: Codex 7d 80%"), "{tip}");
        assert!(tip.contains("5h 12%"), "{tip}");
        assert!(tip.contains("7d 80%"), "{tip}");
    }

    #[test]
    fn codex_headline_preserves_primary_ties_and_unknown_labels() {
        for (primary_minutes, secondary_minutes, primary_pct, secondary_pct, label) in [
            (300, 10080, 90.0, 90.0, "Codex 5h"),
            (10080, 300, 90.0, 90.0, "Codex 7d"),
            (10080, 1440, 20.0, 95.0, "Codex window"),
            // Selection must precede the lossy f32 display conversion.
            (300, 10080, 90.0000002, 90.0000001, "Codex 5h"),
        ] {
            let mut s = Snapshot::empty(chrono::Utc::now());
            let mut q = codex_5h_weekly(primary_pct, secondary_pct);
            q.primary.window_duration_minutes = primary_minutes;
            q.secondary.as_mut().unwrap().window_duration_minutes = secondary_minutes;
            let pct = q.worst_window().unwrap().used_percent.round() as i64;
            s.codex_quota = Some(q);
            let view = TrayView::from_snapshot(&s);
            assert_eq!(tray_title(&view), format!("Codex {pct}%"));
            assert_eq!(bucket_for_view(&view, false), ColorBucket::Red);
            let tip = tray_tooltip(&view, false);
            assert!(
                tip.starts_with(&format!("Balanze - worst: {label} {pct}%")),
                "{tip}"
            );
        }
    }

    /// The original bug: the ring colored from the worst window (weekly 94%) but
    /// the title only ever printed the 5h (20%) - a red icon beside "20%". Now
    /// the ring, title, and tooltip header all name the same worst window.
    #[test]
    fn worst_window_drives_ring_title_and_tooltip_together() {
        use chrono::Utc;
        let mut s = Snapshot::empty(Utc::now());
        s.claude_oauth = Some(oauth_5h_7d(20.0, 94.0));
        let view = TrayView::from_snapshot(&s);

        assert_eq!(bucket_for_view(&view, false), ColorBucket::Red);
        // Menu-bar shows Claude's worst (94), not the 5h (20).
        assert_eq!(tray_title(&view), "Claude 94%");
        let tip = tray_tooltip(&view, false);
        assert!(tip.contains("worst: Claude 7d 94%"), "{tip}");
        assert!(tip.contains("5h 20%"), "{tip}");
        assert!(tip.contains("7d 94%"), "{tip}");
    }

    #[test]
    fn title_shows_both_providers_worst() {
        use chrono::Utc;
        let mut s = Snapshot::empty(Utc::now());
        s.claude_oauth = Some(oauth_5h_7d(20.0, 40.0));
        s.codex_quota = Some(codex_with_util(30.0));
        let view = TrayView::from_snapshot(&s);
        assert_eq!(tray_title(&view), "Claude 40% · Codex 30%");
        // Ring = the largest shown number (40 -> Green, <50).
        assert_eq!(bucket_for_view(&view, false), ColorBucket::Green);
    }

    #[test]
    fn title_omits_absent_provider() {
        use chrono::Utc;
        let mut s = Snapshot::empty(Utc::now());
        s.codex_quota = Some(codex_with_util(55.0));
        let view = TrayView::from_snapshot(&s);
        assert_eq!(tray_title(&view), "Codex 55%");
    }

    #[test]
    fn ring_classifies_rounded_worst_at_cutoff() {
        use chrono::Utc;
        // 89.6 displays "90%" and must read Red, not Orange.
        let mut s = Snapshot::empty(Utc::now());
        s.claude_oauth = Some(oauth_5h_7d(10.0, 89.6));
        let view = TrayView::from_snapshot(&s);
        assert_eq!(bucket_for_view(&view, false), ColorBucket::Red);
        assert_eq!(tray_title(&view), "Claude 90%");
    }

    #[test]
    fn cold_start_vs_degraded_tooltip() {
        use chrono::Utc;
        let snapshot = Snapshot::empty(Utc::now());
        let view = TrayView::from_snapshot(&snapshot);
        assert_eq!(tray_tooltip(&view, false), "Balanze - connecting...");
        assert_eq!(tray_tooltip(&view, true), "Balanze - quota unavailable");
    }

    #[test]
    fn weekly_variant_cadences_fold_into_7d() {
        use chrono::Utc;
        // A per-model weekly bucket (seven_day_opus) at 80% must surface as the
        // 7d figure and drive the ring, not vanish behind the named windows.
        let mut s = Snapshot::empty(Utc::now());
        let mut oauth = oauth_with_util(15.0); // five_hour at 15%
        oauth.cadences.push(anthropic_oauth::CadenceBar {
            key: "seven_day_opus".into(),
            display_label: "7d Opus".into(),
            utilization_percent: 80.0,
            resets_at: Utc::now(),
        });
        s.claude_oauth = Some(oauth);
        let view = TrayView::from_snapshot(&s);
        assert_eq!(view.claude_7d, Some(("7d-opus", 80.0)));
        assert_eq!(tray_title(&view), "Claude 80%");
        assert_eq!(bucket_for_view(&view, false), ColorBucket::Orange);
    }

    #[test]
    fn tooltip_fits_windows_128_char_cap() {
        use chrono::Utc;
        // The Windows tray tooltip is capped near 128 chars; the fixed layout
        // must stay well under even with both providers present and degraded.
        let mut s = Snapshot::empty(Utc::now());
        s.claude_oauth = Some(oauth_5h_7d(88.0, 94.0));
        s.codex_quota = Some(codex_5h_weekly(88.0, 94.0));
        let view = TrayView::from_snapshot(&s);
        let tip = tray_tooltip(&view, true);
        assert!(tip.len() <= 128, "tooltip {} chars: {tip}", tip.len());
    }

    #[test]
    fn separate_mode_isolates_stale_codex_and_combined_discloses_it() {
        let mut s = Snapshot::empty(chrono::Utc::now());
        s.claude_oauth = Some(fresh_oauth(94.0, 20.0));
        s.codex_quota = Some(codex_5h_weekly(10.0, 20.0));
        s.codex_quota_error = Some("offline".into());
        s.fetched_at = chrono::Utc::now();
        let mut settings = settings::Settings::default();
        let targets = paint_targets(&s, &settings);
        assert_eq!(targets.main.icon.bucket, ColorBucket::Red);
        assert_eq!(
            targets.codex.unwrap().icon.state,
            tray_icon::IconState::Stale
        );
        settings.tray.mode = settings::TrayMode::Combined;
        let target = paint_targets(&s, &settings);
        assert!(target.codex.is_none());
        assert_eq!(target.main.icon.provider, tray_icon::Provider::Claude);
        assert_eq!(target.main.icon.state, tray_icon::IconState::Partial);
        assert!(target.main.tooltip.contains("stale"));
    }

    #[test]
    fn settings_change_mode_percentages_and_disabled_codex() {
        let mut s = Snapshot::empty(chrono::Utc::now());
        s.claude_oauth = Some(fresh_oauth(32.0, 78.0));
        s.fetched_at = chrono::Utc::now();
        s.codex_quota = Some(codex_5h_weekly(10.0, 78.0));
        s.fetched_at = chrono::Utc::now();
        let mut settings = settings::Settings::default();
        assert_eq!(paint_targets(&s, &settings).main.title, "Cla");
        settings.tray.show_percentages = true;
        assert_eq!(paint_targets(&s, &settings).main.title, "Cla 78%");
        settings.tray.mode = settings::TrayMode::Combined;
        assert_eq!(
            paint_targets(&s, &settings).main.icon.provider,
            tray_icon::Provider::Codex
        );
        settings.providers.codex_enabled = false;
        let target = paint_targets(&s, &settings);
        assert!(target.codex.is_none());
        assert_eq!(target.main.icon.provider, tray_icon::Provider::Claude);
    }
}
