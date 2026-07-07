//! Production sink for the `state_coordinator` actor (AGENTS.md §4 #7 - the
//! only crate that may call Tauri tray APIs). Emits `usage_updated` /
//! `degraded_state` to the Svelte UI and repaints the gauge tray icon,
//! deduped by `(ColorBucket, title, tooltip)` per AGENTS.md §3.1.

use serde::Serialize;
use state_coordinator::{STATUSLINE_FRESHNESS_SECS, Sink, Snapshot, Source};
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

/// The statusLine payload feeds the tray only while fresh. Mirrors the
/// coordinator's ingest guard and the popover's render-time check
/// (`STATUSLINE_FRESHNESS_MS` in `quota.ts`): a frozen file (another tool owns
/// the single `statusLine` slot, so Balanze's writer never refreshes it) must
/// not drive the tray heat as if live. Age is `fetched_at - captured_at` -
/// `fetched_at` is re-stamped on every coordinator emit, so this is a pure,
/// wall-clock-free measure consistent with both peer checks. This is the tray's
/// own guard, independent of the coordinator's error slot (belt-and-suspenders):
/// even if the ingest marker regressed, a stale window still can't heat the tray.
fn statusline_fresh(s: &Snapshot) -> bool {
    s.claude_statusline.as_ref().is_some_and(|sl| {
        // `.num_seconds()` on the TimeDelta keeps this free of a direct `chrono`
        // dependency (chrono is dev-only in src-tauri); the method resolves via
        // the re-exported `DateTime<Utc>` fields on `Snapshot`. Fresh iff the age
        // is within `[0, threshold]` - a negative age (future-dated captured_at,
        // clock skew) is not trusted, matching the coordinator/popover guards.
        let age_secs = s
            .fetched_at
            .signed_duration_since(sl.captured_at)
            .num_seconds();
        (0..=STATUSLINE_FRESHNESS_SECS).contains(&age_secs)
    })
}

/// The tray's canonical windows, per provider: Claude 5h, Claude weekly (7d =
/// the worst of every non-5h cadence), and Codex primary. The ring color, the
/// menu-bar title, and the tooltip ALL derive from this one view, so a red ring
/// always corresponds to a number the user can see. (The old bug: the ring came
/// from the worst window across everything, but the title only ever printed the
/// 5h - a red icon could sit next to "C 20%".) Folds OAuth cadences and fresh
/// statusline windows; a stale statusline is excluded (see `statusline_fresh`).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct TrayView {
    claude_5h: Option<f32>,
    claude_7d: Option<f32>,
    codex: Option<f32>,
}

fn fold_max(slot: &mut Option<f32>, v: f32) {
    *slot = Some(slot.map_or(v, |cur| cur.max(v)));
}

impl TrayView {
    fn from_snapshot(s: &Snapshot) -> Self {
        let mut v = TrayView::default();
        if let Some(o) = &s.claude_oauth {
            for c in &o.cadences {
                if c.key == "five_hour" {
                    fold_max(&mut v.claude_5h, c.utilization_percent);
                } else {
                    // Every non-5h cadence is a weekly variant; folding them all
                    // into 7d guarantees no window can hide from the ring/title.
                    fold_max(&mut v.claude_7d, c.utilization_percent);
                }
            }
        }
        if statusline_fresh(s) {
            if let Some(rl) = s
                .claude_statusline
                .as_ref()
                .and_then(|sl| sl.payload.rate_limits.as_ref())
            {
                for w in &rl.windows {
                    if w.key == "five_hour" {
                        fold_max(&mut v.claude_5h, w.used_percent);
                    } else {
                        fold_max(&mut v.claude_7d, w.used_percent);
                    }
                }
            }
        }
        v.codex = s
            .codex_quota
            .as_ref()
            .map(|q| q.primary.used_percent as f32);
        v
    }

    /// Any live quota signal at all - distinguishes real data (paint a heat
    /// color) from a cold-start snapshot (paint Neutral).
    fn has_data(&self) -> bool {
        self.claude_5h.is_some() || self.claude_7d.is_some() || self.codex.is_some()
    }

    /// Claude's worst window (max of 5h and 7d) - the single Claude figure shown
    /// in the menu-bar title.
    fn claude_worst(&self) -> Option<f32> {
        match (self.claude_5h, self.claude_7d) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// The single worst window across providers, labeled - drives the ring color
    /// and the tooltip header. Compared on the raw value; callers round for
    /// display. Because every cadence folds into 5h/7d/Codex, this is the same
    /// maximum the ring paints, so color and shown number cannot disagree.
    fn worst(&self) -> Option<(&'static str, f32)> {
        [
            ("Claude 5h", self.claude_5h),
            ("Claude 7d", self.claude_7d),
            ("Codex", self.codex),
        ]
        .into_iter()
        .filter_map(|(label, pct)| pct.map(|p| (label, p)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Rounded utilization of the worst window; the ring classifies THIS so its
    /// color matches the largest number the title/tooltip display (89.6 shows
    /// "90%" and must read Red, not Orange).
    fn worst_rounded(&self) -> f32 {
        self.worst().map_or(0.0, |(_, p)| p.round())
    }
}

/// Pick the tray color for a view. A degraded source forces the warning bucket;
/// a view with no quota signal yet is Neutral; otherwise the worst window's
/// rounded utilization maps to a heat color.
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
/// providers in fixed slots, each showing that provider's worst window. The ring
/// color matches the larger of the two shown numbers. An unconfigured provider
/// is omitted.
fn tray_title(view: &TrayView) -> String {
    let c = view
        .claude_worst()
        .map(|p| format!("Claude {}%", p.round() as i64));
    let o = view.codex.map(|p| format!("Codex {}%", p.round() as i64));
    [c, o].into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

/// Tooltip: a fixed-layout status panel. A header names the worst window so the
/// ring color is explained, then both providers in stable slots. Kept compact
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
    if let Some(p) = view.claude_7d {
        claude.push(format!("7d {}%", p.round() as i64));
    }
    if !claude.is_empty() {
        lines.push(format!("Claude  {}", claude.join("  ")));
    }
    if let Some(p) = view.codex {
        lines.push(format!("Codex   {}%", p.round() as i64));
    }
    if degraded {
        lines.push("(some data may be stale)".to_string());
    }
    lines.join("\n")
}

pub(crate) struct TauriSink {
    app: AppHandle,
    last_painted: Option<(ColorBucket, String, String)>,
}

impl TauriSink {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self {
            app,
            last_painted: None,
        }
    }

    fn paint_target(&self, s: &Snapshot, degraded: bool) -> (ColorBucket, String, String) {
        let view = TrayView::from_snapshot(s);
        (
            bucket_for_view(&view, degraded),
            tray_title(&view),
            tray_tooltip(&view, degraded),
        )
    }
}

/// True if any source's error slot is set. Used to keep the tray on the
/// warning bucket while ANY source is degraded - a later success from a
/// different source must not clear the warning while another source is still
/// failing (the bug: `on_snapshot` previously hard-coded `degraded = false`).
fn any_source_degraded(s: &Snapshot) -> bool {
    s.claude_oauth_error.is_some()
        || s.claude_jsonl_error.is_some()
        || s.anthropic_api_cost_error.is_some()
        || s.codex_quota_error.is_some()
        || s.openai_error.is_some()
        || s.claude_statusline_error.is_some()
}

impl Sink for TauriSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        if let Err(e) = self.app.emit("usage_updated", snapshot) {
            tracing::warn!("tauri_sink: emit usage_updated failed: {e}");
        }
        let target = self.paint_target(snapshot, any_source_degraded(snapshot));
        if self.last_painted.as_ref() != Some(&target) {
            tray_icon::paint(&self.app, target.0, &target.1, &target.2);
            self.last_painted = Some(target);
        }
    }

    fn on_degraded(&mut self, source: Source, error: &str) {
        let payload = DegradedPayload {
            source: source_key(source).to_string(),
            error: error.to_string(),
        };
        if let Err(e) = self.app.emit("degraded_state", payload) {
            tracing::warn!("tauri_sink: emit degraded_state failed: {e}");
        }
        let (title, tooltip) = self
            .last_painted
            .as_ref()
            .map(|(_, t, tip)| (t.clone(), tip.clone()))
            .unwrap_or_default();
        let target = (ColorBucket::Warn, title, tooltip);
        if self.last_painted.as_ref() != Some(&target) {
            tray_icon::paint(&self.app, target.0, &target.1, &target.2);
            self.last_painted = Some(target);
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
    fn any_source_error_keeps_warning() {
        use chrono::Utc;
        let mut s = Snapshot::empty(Utc::now());
        assert!(!any_source_degraded(&s));
        s.openai_error = Some("HTTP 500".into());
        assert!(any_source_degraded(&s));
    }

    #[test]
    fn bucket_thresholds() {
        assert_eq!(ColorBucket::from_util(0.0), ColorBucket::Green);
        assert_eq!(ColorBucket::from_util(49.9), ColorBucket::Green);
        assert_eq!(ColorBucket::from_util(50.0), ColorBucket::Yellow);
        assert_eq!(ColorBucket::from_util(74.9), ColorBucket::Yellow);
        assert_eq!(ColorBucket::from_util(75.0), ColorBucket::Orange);
        assert_eq!(ColorBucket::from_util(90.0), ColorBucket::Red);
        assert_eq!(ColorBucket::from_util(150.0), ColorBucket::Red);
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
        let mut s = Snapshot::empty(Utc::now());
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
            Utc::now(),
        ));
        assert_eq!(worst_utilization(&s), 95.0);
        assert!(has_quota_data(&s));
    }

    /// A stale statusLine payload (frozen file - another tool owns the slot)
    /// must NOT heat the tray or count as a live quota signal, even at 95%.
    /// The tray's own freshness guard, independent of the coordinator's error
    /// slot (belt-and-suspenders).
    #[test]
    fn stale_statusline_does_not_drive_worst_utilization() {
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
        assert_eq!(
            worst_utilization(&s),
            0.0,
            "stale statusline must not heat the tray"
        );
        assert!(
            !has_quota_data(&s),
            "stale statusline is not a live quota signal"
        );
    }

    /// A future-dated payload (captured_at ahead of fetched_at - the clock moved
    /// backward after the write) is equally untrusted: an upper-bound-only check
    /// would treat it as fresh and let it heat the tray.
    #[test]
    fn future_dated_statusline_does_not_drive_worst_utilization() {
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
        assert_eq!(worst_utilization(&s), 0.0, "future-dated must not heat");
        assert!(!has_quota_data(&s), "future-dated is not a live signal");
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

    fn codex_with_util(util: f64) -> codex_local::CodexQuotaSnapshot {
        use chrono::Utc;
        codex_local::CodexQuotaSnapshot {
            observed_at: Utc::now(),
            session_id: "test".into(),
            primary: codex_local::RateLimitWindow {
                used_percent: util,
                window_duration_minutes: 10080,
                resets_at: Utc::now(),
            },
            secondary: None,
            plan_type: "go".into(),
            rate_limit_reached: false,
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
        let view = TrayView::from_snapshot(&Snapshot::empty(Utc::now()));
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
        assert_eq!(view.claude_7d, Some(80.0));
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
        s.codex_quota = Some(codex_with_util(72.0));
        let view = TrayView::from_snapshot(&s);
        let tip = tray_tooltip(&view, true);
        assert!(tip.len() <= 128, "tooltip {} chars: {tip}", tip.len());
    }
}
