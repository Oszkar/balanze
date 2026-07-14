use chrono::{DateTime, Duration, Utc};

use crate::style::apply_style;

/// Cross-provider data for the statusline. Populated from the watcher snapshot
/// or self-compose in later PRs; `None` in PR1 (placeholders render empty).
#[derive(Debug, Clone, Default)]
pub struct CrossProvider {
    /// Local Codex 5-hour window utilization (0..100). `None` if absent.
    pub codex_five_hour: Option<f32>,
    /// Local Codex weekly window utilization (0..100). `None` if absent.
    pub codex_weekly: Option<f32>,
    pub openai_cost_micro_usd: Option<i64>,
    /// True when the Codex figure is stale (e.g. an old snapshot). The
    /// self-compose path reads Codex locally each turn, so it is false there.
    pub codex_stale: bool,
    /// True when the OpenAI figure is stale (old snapshot, or a cached value
    /// served because a fresh fetch failed / is in cooldown).
    pub openai_stale: bool,
}

/// Everything `render` needs. Borrowed; `render` is pure and allocates only the
/// output string.
pub struct RenderInput<'a> {
    pub snapshot: &'a claude_statusline::StatuslineSnapshot,
    pub cross: Option<&'a CrossProvider>,
    pub config: &'a settings::StatuslineConfig,
    pub now: DateTime<Utc>,
    /// Emit ANSI color. The CLI sets this from NO_COLOR only (NOT TTY
    /// detection): Claude Code captures statusline stdout and renders ANSI even
    /// though it is not a terminal.
    pub color: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Base,
    Warn,
    Critical,
}

/// Render the configured lines. Empty segments are dropped and whitespace
/// collapsed; empty lines are omitted; lines join with `\n`.
pub fn render(input: &RenderInput) -> String {
    input
        .config
        .lines
        .iter()
        .map(|tmpl| fill_line(tmpl, input))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Substitute `{key}` placeholders in one template. Each token is either a
/// `{segment}` placeholder (replaced by its rendered value, dropped if empty)
/// or literal text (kept). Segment values may contain spaces - they are
/// inserted whole, so internal spacing is preserved while inter-segment gaps
/// collapse to one space.
///
/// LITERAL (non-placeholder) tokens are always kept, while `{placeholder}`
/// tokens are dropped when their segment is empty/None. A custom template that
/// mixes literals with an absent segment (e.g. `"cost: {cost}"`) can therefore
/// leave a dangling literal (`"cost:"`) when that segment renders empty.
fn fill_line(template: &str, input: &RenderInput) -> String {
    let mut parts: Vec<String> = Vec::new();
    for tok in template.split_whitespace() {
        if let Some(key) = tok.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
            if let Some(v) = render_segment(key, input) {
                if !v.is_empty() {
                    parts.push(v);
                }
            }
            // Unknown key or empty value -> drop the token.
        } else {
            parts.push(tok.to_string());
        }
    }
    parts.join(" ")
}

/// Render a single segment by key. `None` -> the segment is omitted entirely.
fn render_segment(key: &str, input: &RenderInput) -> Option<String> {
    let snap = input.snapshot;
    let segs = &input.config.segments;
    let theme = input.config.theme.as_str();
    match key {
        "model" => {
            let name = snap.model_display_name.as_deref()?;
            Some(paint(
                &format!("🤖 {name}"),
                resolve(&segs.model.style, theme, "model", Tone::Base),
                "",
                "",
                Tone::Base,
                input.color,
            ))
        }
        // agent parsing is deferred (no `agent` field in a normal payload).
        "agent" => None,
        "context_bar" => {
            let pct = snap.context_used_percent?;
            let c = &segs.context_bar;
            let tone = tone_pct(pct, c.warn, c.critical);
            let shown = pct.round() as i64;
            let text = format!("🧠 {} {shown}%", bar(pct, c.width));
            Some(paint(
                &text,
                resolve(&c.style, theme, "context_bar", Tone::Base),
                resolve(&c.warn_style, theme, "context_bar", Tone::Warn),
                resolve(&c.critical_style, theme, "context_bar", Tone::Critical),
                tone,
                input.color,
            ))
        }
        "cost" => {
            let micro = snap.session_cost_micro_usd?;
            let c = &segs.cost;
            let tone = tone_money(micro, c.warn_micro_usd, c.critical_micro_usd);
            // `~` marks this as the Claude session ESTIMATE, not billed spend -
            // it must stay distinguishable from a real `OpenAI $` segment.
            Some(paint(
                &format!("💰 ~{}", fmt_money(micro)),
                resolve(&c.style, theme, "cost", Tone::Base),
                resolve(&c.warn_style, theme, "cost", Tone::Warn),
                resolve(&c.critical_style, theme, "cost", Tone::Critical),
                tone,
                input.color,
            ))
        }
        "usage" => render_usage(input),
        "codex" => {
            let cross = input.cross?;
            let mut windows = Vec::new();
            if let Some(pct) = cross.codex_five_hour {
                windows.push(render_codex_window("🌀 5h", pct, input));
            }
            if let Some(pct) = cross.codex_weekly {
                // "7d", not "wk": every other surface calls this window 7d, and
                // the provider glyph is what separates it from Claude's 7d.
                windows.push(render_codex_window("🌀 7d", pct, input));
            }
            if windows.is_empty() {
                return None;
            }
            let mark = if cross.codex_stale { " ⚠️" } else { "" };
            Some(format!("{}{mark}", windows.join(" ")))
        }
        "openai_cost" => {
            let cross = input.cross?;
            let micro = cross.openai_cost_micro_usd?;
            let mark = if cross.openai_stale { " ⚠️" } else { "" };
            Some(paint(
                &format!("🌀 {}{mark}", fmt_money(micro)),
                resolve(&segs.openai_cost.style, theme, "openai_cost", Tone::Base),
                "",
                "",
                Tone::Base,
                input.color,
            ))
        }
        _ => None,
    }
}

/// Render the 5h + 7d windows as one segment, each window independently toned.
fn render_usage(input: &RenderInput) -> Option<String> {
    let rl = input.snapshot.rate_limits.as_ref()?;
    let c = &input.config.segments.usage;
    let mut windows: Vec<String> = Vec::new();
    if let Some(w) = rl.five_hour() {
        windows.push(render_window("✳️ 5h", w, Duration::hours(5), c, input));
    }
    if let Some(w) = rl.seven_day() {
        windows.push(render_window("✳️ 7d", w, Duration::days(7), c, input));
    }
    if windows.is_empty() {
        None
    } else {
        Some(windows.join(" "))
    }
}

fn render_window(
    label: &str,
    w: &claude_statusline::RateWindow,
    window_len: Duration,
    c: &settings::statusline::UsageSegment,
    input: &RenderInput,
) -> String {
    let shown = w.used_percent.round() as i64;
    let mut text = format!("{label} {shown}%");
    if c.show_pace {
        let p = window::pace(w.used_percent as f64, w.resets_at, window_len, input.now);
        if let Some(ratio) = p.ratio {
            let arrow = if ratio >= 1.0 { '↑' } else { '↓' };
            text.push_str(&format!(" {arrow}{ratio:.1}×"));
        }
    }
    if c.show_reset {
        let delta = w.resets_at - input.now;
        text.push_str(&format!(" ({})", fmt_countdown(delta)));
    }
    // Color by the shared 50/75/90 severity classifier so the statusline agrees
    // with the tray, popover, and CLI. Classify the ROUNDED display value
    // (`shown`), not the raw percent, so the number and color never disagree at
    // a cutoff (89.6 shows "90%" and must read Red, not Orange). The per-segment
    // warn/critical config is NOT consulted for color here; it stays in
    // `settings` as the hook for future user-configurable thresholds.
    let style = severity_style(
        input.config.theme.as_str(),
        window::Severity::from_util(shown as f32),
    );
    if input.color {
        apply_style(style, &text)
    } else {
        text
    }
}

/// One Codex window, severity-toned by the shared 50/75/90 classifier (same
/// scale as the usage windows and the tray/popover/CLI). Classify the ROUNDED
/// display value so the number and color never disagree at a cutoff.
fn render_codex_window(label: &str, pct: f32, input: &RenderInput) -> String {
    let shown = pct.round() as i64;
    let text = format!("{label} {shown}%");
    let style = severity_style(
        input.config.theme.as_str(),
        window::Severity::from_util(shown as f32),
    );
    if input.color {
        apply_style(style, &text)
    } else {
        text
    }
}

/// Apply the tone's configured style to `text`, gated by `color`. A blank style
/// string (or `color=false`) returns the text unchanged.
fn paint(text: &str, base: &str, warn: &str, crit: &str, tone: Tone, color: bool) -> String {
    if !color {
        return text.to_string();
    }
    let spec = match tone {
        Tone::Base => base,
        Tone::Warn => warn,
        Tone::Critical => crit,
    };
    apply_style(spec, text)
}

/// Effective style spec for a segment+tone: a non-empty config override wins; an
/// empty override falls through to the `theme` palette. This is what makes a
/// partial settings override (changing only a width or threshold) keep its
/// coloring, and what makes `theme` actually switch colors.
fn resolve<'a>(over: &'a str, theme: &str, segment: &'static str, tone: Tone) -> &'a str {
    if over.is_empty() {
        palette_style(theme, segment, tone)
    } else {
        over
    }
}

/// Curated default style for a (theme, segment, tone). Dark is the fallback for
/// any unrecognized theme (e.g. a typo). Warn and critical are shared across
/// segments per theme; the base tone differs per segment. Dark values reproduce
/// the pre-theme hard-coded defaults exactly; light is a tokyonight-light set.
fn palette_style(theme: &str, segment: &str, tone: Tone) -> &'static str {
    let light = theme.eq_ignore_ascii_case("light");
    match tone {
        Tone::Warn => {
            if light {
                "fg:#8f5e15"
            } else {
                "fg:#e0af68"
            }
        }
        Tone::Critical => {
            if light {
                "bold fg:#8c4351"
            } else {
                "bold fg:#f7768e"
            }
        }
        Tone::Base => match (segment, light) {
            ("model", false) => "bold fg:#7aa2f7",
            ("model", true) => "bold fg:#34548a",
            ("agent", false) => "fg:#9ece6a",
            ("agent", true) => "fg:#485e30",
            ("context_bar", false) => "fg:#7dcfff",
            ("context_bar", true) => "fg:#166775",
            // cost, openai_cost, and any unknown segment -> neutral fg. (usage
            // and codex are colored by the severity classifier, not palette_style.)
            (_, false) => "fg:#a9b1d6",
            (_, true) => "fg:#343b58",
        },
    }
}

/// Color for a utilization severity band, per theme. The usage windows and the
/// Codex segment are shaded by the shared `window::Severity` classifier
/// (50 / 75 / 90), NOT the per-segment config thresholds, so the statusline
/// agrees with the tray, popover, and CLI. Yellow/Red reuse the existing
/// warn/critical hues; Green/Orange extend the palette (tokyonight-family),
/// dark + light.
fn severity_style(theme: &str, sev: window::Severity) -> &'static str {
    let light = theme.eq_ignore_ascii_case("light");
    match sev {
        window::Severity::Green => {
            if light {
                "fg:#485e30"
            } else {
                "fg:#9ece6a"
            }
        }
        window::Severity::Yellow => {
            if light {
                "fg:#8f5e15"
            } else {
                "fg:#e0af68"
            }
        }
        window::Severity::Orange => {
            if light {
                "fg:#a1521a"
            } else {
                "fg:#ff9e64"
            }
        }
        window::Severity::Red => {
            if light {
                "bold fg:#8c4351"
            } else {
                "bold fg:#f7768e"
            }
        }
    }
}

fn tone_pct(pct: f32, warn: u32, critical: u32) -> Tone {
    let p = pct.round() as i64;
    if p >= critical as i64 {
        Tone::Critical
    } else if p >= warn as i64 {
        Tone::Warn
    } else {
        Tone::Base
    }
}

fn tone_money(micro: i64, warn: i64, critical: i64) -> Tone {
    if micro >= critical {
        Tone::Critical
    } else if micro >= warn {
        Tone::Warn
    } else {
        Tone::Base
    }
}

/// micro-USD -> "$X.XX". f64 only at this display boundary (AGENTS.md §2.1).
fn fmt_money(micro: i64) -> String {
    format!("${:.2}", micro as f64 / 1_000_000.0)
}

/// ASCII utilization bar of `width` cells, e.g. "[####------]".
fn bar(pct: f32, width: u32) -> String {
    let w = width.max(1);
    let filled = ((pct / 100.0) * w as f32).round().clamp(0.0, w as f32) as u32;
    let empty = w - filled;
    format!(
        "[{}{}]",
        "#".repeat(filled as usize),
        "-".repeat(empty as usize)
    )
}

/// Compact reset countdown: "1h23m", "3d4h", "12m". Past/zero -> "0m".
fn fmt_countdown(delta: Duration) -> String {
    let secs = delta.num_seconds().max(0);
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m}m")
    } else {
        format!("{m}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn cfg() -> settings::StatuslineConfig {
        settings::StatuslineConfig::default()
    }

    // now = a fixed instant; 5h resets 1h23m later, 7d resets ~5d later.
    fn now() -> chrono::DateTime<Utc> {
        Utc.timestamp_opt(1_767_225_600, 0).single().unwrap()
    }

    fn snap() -> claude_statusline::StatuslineSnapshot {
        claude_statusline::StatuslineSnapshot {
            rate_limits: Some(claude_statusline::RateLimits {
                windows: vec![
                    claude_statusline::RateWindow {
                        key: "five_hour".to_string(),
                        label: "5-hour".to_string(),
                        used_percent: 82.0,
                        resets_at: now() + chrono::Duration::minutes(83),
                    },
                    claude_statusline::RateWindow {
                        key: "seven_day".to_string(),
                        label: "7-day".to_string(),
                        used_percent: 88.0,
                        resets_at: now() + chrono::Duration::days(5),
                    },
                ],
            }),
            session_cost_micro_usd: Some(2_500_000),
            claude_code_version: None,
            model_display_name: Some("Opus".to_string()),
            context_used_percent: Some(42.0),
        }
    }

    #[test]
    fn renders_default_layout_plain() {
        let c = cfg();
        let s = snap();
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: false,
        });
        assert!(out.contains("🤖 Opus"), "model: {out}");
        assert!(out.contains("5h 82%"), "5h pct: {out}");
        assert!(out.contains("(1h23m)"), "5h countdown: {out}");
        assert!(out.contains('↑'), "pace arrow over pace: {out}");
        assert!(out.contains("7d 88%"), "7d pct: {out}");
        assert!(out.contains("💰 ~$2.50"), "cost (estimate-marked): {out}");
        assert!(!out.contains("Codex"), "codex empty in PR1: {out}");
        assert!(!out.contains("OpenAI"), "openai empty in PR1: {out}");
    }

    #[test]
    fn absent_segments_collapse_no_artifacts() {
        let c = cfg();
        let s = claude_statusline::StatuslineSnapshot {
            rate_limits: None,
            session_cost_micro_usd: None,
            claude_code_version: None,
            model_display_name: None,
            context_used_percent: None,
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: false,
        });
        assert!(!out.contains('{'), "no unfilled placeholders: {out:?}");
        assert!(!out.contains("  "), "no double spaces: {out:?}");
        assert!(!out.contains("\n\n"), "no blank lines: {out:?}");
    }

    #[test]
    fn render_is_pure() {
        let c = cfg();
        let s = snap();
        let mk = || {
            render(&RenderInput {
                snapshot: &s,
                cross: None,
                config: &c,
                now: now(),
                color: false,
            })
        };
        assert_eq!(mk(), mk());
    }

    #[test]
    fn display_percent_uses_round_half_away_matching_tone() {
        // 82.5% must display "83%" (round half away from zero), matching the
        // tone computation, so the shown number and the color never disagree.
        let c = cfg();
        let mut s = snap();
        s.rate_limits
            .as_mut()
            .unwrap()
            .windows
            .iter_mut()
            .find(|w| w.key == "five_hour")
            .unwrap()
            .used_percent = 82.5;
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: false,
        });
        assert!(
            out.contains("5h 83%"),
            "82.5 must round to 83 in display: {out}"
        );
    }

    #[test]
    fn no_pace_arrow_right_after_reset() {
        // A window whose resets_at is exactly now + window_len has elapsed_fraction
        // 0, so window::pace returns ratio None -> no arrow. Set BOTH windows so
        // no arrow appears anywhere.
        let c = cfg();
        let n = now();
        let mut s = snap();
        {
            let rl = s.rate_limits.as_mut().unwrap();
            rl.windows
                .iter_mut()
                .find(|w| w.key == "five_hour")
                .unwrap()
                .resets_at = n + chrono::Duration::hours(5);
            rl.windows
                .iter_mut()
                .find(|w| w.key == "seven_day")
                .unwrap()
                .resets_at = n + chrono::Duration::days(7);
        }
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: n,
            color: false,
        });
        assert!(
            !out.contains('↑') && !out.contains('↓'),
            "no pace arrow when ratio is None: {out}"
        );
    }

    #[test]
    fn usage_flags_suppress_pace_and_reset() {
        let mut c = cfg();
        c.segments.usage.show_pace = false;
        c.segments.usage.show_reset = false;
        let s = snap();
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: false,
        });
        assert!(out.contains("5h 82%"), "{out}");
        assert!(
            !out.contains('↑') && !out.contains('↓'),
            "no pace arrow when show_pace=false: {out}"
        );
        assert!(
            !out.contains('('),
            "no reset countdown when show_reset=false: {out}"
        );
    }

    #[test]
    fn color_false_has_no_escapes() {
        let c = cfg();
        let s = snap();
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: false,
        });
        assert!(!out.contains('\x1b'), "no ANSI when color=false: {out:?}");
    }

    #[test]
    fn color_true_wraps_toned_segments() {
        let c = cfg();
        // 5h at 95% -> critical (>=90); default critical_style = "bold fg:#f7768e".
        let mut s = snap();
        s.rate_limits
            .as_mut()
            .unwrap()
            .windows
            .iter_mut()
            .find(|w| w.key == "five_hour")
            .unwrap()
            .used_percent = 95.0;
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: true,
        });
        assert!(
            out.contains('\x1b'),
            "ANSI present when color=true: {out:?}"
        );
        // bold + truecolor #f7768e = rgb(247,118,142)
        assert!(
            out.contains("\x1b[1;38;2;247;118;142m"),
            "critical style applied to 5h: {out:?}"
        );
    }

    #[test]
    fn blank_styles_resolve_to_theme_palette() {
        // Blank per-segment styles are NOT "no color" - they resolve to the
        // theme palette (dark by default). This is what lets a partial override
        // (changing only a width or threshold) keep its coloring. The default
        // config has all styles blank.
        let c = cfg();
        let s = snap();
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: true,
        });
        assert!(
            out.contains('\x1b'),
            "blank styles must resolve to themed ANSI: {out:?}"
        );
        // model is always base tone; dark base = "bold fg:#7aa2f7" = rgb(122,162,247).
        assert!(
            out.contains("\x1b[1;38;2;122;162;247m"),
            "model uses the dark base palette: {out:?}"
        );
    }

    #[test]
    fn theme_light_changes_colors() {
        // Setting theme=light (with default blank styles) must swap the palette.
        let mut c = cfg();
        c.theme = "light".to_string();
        let s = snap();
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: true,
        });
        // light model base = "bold fg:#34548a" = rgb(52,84,138).
        assert!(
            out.contains("\x1b[1;38;2;52;84;138m"),
            "light theme model color present: {out:?}"
        );
        // the dark model color must be gone.
        assert!(
            !out.contains("\x1b[1;38;2;122;162;247m"),
            "dark model color must not appear under the light theme: {out:?}"
        );
    }

    #[test]
    fn explicit_style_overrides_theme() {
        // A non-empty per-segment style wins over the theme palette.
        let mut c = cfg();
        c.segments.model.style = "fg:#010203".to_string();
        let s = snap();
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: true,
        });
        assert!(
            out.contains("\x1b[38;2;1;2;3m"),
            "explicit model override used instead of the palette: {out:?}"
        );
    }

    #[test]
    fn color_true_shades_yellow_band_50_to_75() {
        // 5h at 60% -> Yellow (>=50, <75); yellow = fg:#e0af68 = rgb(224,175,104).
        let c = cfg();
        let mut s = snap();
        s.rate_limits
            .as_mut()
            .unwrap()
            .windows
            .iter_mut()
            .find(|w| w.key == "five_hour")
            .unwrap()
            .used_percent = 60.0;
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: true,
        });
        assert!(
            out.contains("\x1b[38;2;224;175;104m"),
            "yellow band applied: {out:?}"
        );
    }

    #[test]
    fn color_true_greens_below_50() {
        // 5h at 40% -> Green (<50); green = fg:#9ece6a = rgb(158,206,106). The
        // statusline greens up when there is headroom (cross-surface parity).
        let c = cfg();
        let mut s = snap();
        s.rate_limits
            .as_mut()
            .unwrap()
            .windows
            .iter_mut()
            .find(|w| w.key == "five_hour")
            .unwrap()
            .used_percent = 40.0;
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: true,
        });
        assert!(
            out.contains("\x1b[38;2;158;206;106m"),
            "green band below 50: {out:?}"
        );
    }

    #[test]
    fn statusline_matches_cross_surface_rounded_threshold_table() {
        let cases = [
            (49.4, "\x1b[38;2;158;206;106m"),
            (49.5, "\x1b[38;2;224;175;104m"),
            (74.4, "\x1b[38;2;224;175;104m"),
            (74.5, "\x1b[38;2;255;158;100m"),
            (89.4, "\x1b[38;2;255;158;100m"),
            (89.5, "\x1b[1;38;2;247;118;142m"),
        ];

        for (percent, expected_style) in cases {
            let c = cfg();
            let mut s = snap();
            for window in &mut s.rate_limits.as_mut().unwrap().windows {
                window.used_percent = percent;
            }
            let out = render(&RenderInput {
                snapshot: &s,
                cross: None,
                config: &c,
                now: now(),
                color: true,
            });
            assert!(
                out.contains(expected_style),
                "percent={percent}, output={out:?}"
            );
        }
    }

    #[test]
    fn color_true_oranges_band_75_to_90() {
        // 5h at 80% -> Orange (>=75, <90); orange = fg:#ff9e64 = rgb(255,158,100).
        let c = cfg();
        let mut s = snap();
        s.rate_limits
            .as_mut()
            .unwrap()
            .windows
            .iter_mut()
            .find(|w| w.key == "five_hour")
            .unwrap()
            .used_percent = 80.0;
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: true,
        });
        assert!(
            out.contains("\x1b[38;2;255;158;100m"),
            "orange band 75-90: {out:?}"
        );
    }

    #[test]
    fn severity_classifies_rounded_display_value_at_cutoff() {
        // 89.6% displays "90%" and must read Red, not Orange: classify the
        // rounded display value so the number and color never disagree.
        let c = cfg();
        let mut s = snap();
        s.rate_limits
            .as_mut()
            .unwrap()
            .windows
            .iter_mut()
            .find(|w| w.key == "five_hour")
            .unwrap()
            .used_percent = 89.6;
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: true,
        });
        assert!(out.contains("5h 90%"), "89.6 rounds to 90 in label: {out}");
        // Red = bold fg:#f7768e = rgb(247,118,142).
        assert!(
            out.contains("\x1b[1;38;2;247;118;142m"),
            "89.6 shows 90% and must read Red, not Orange: {out:?}"
        );
    }

    #[test]
    fn codex_segment_colored_by_severity_band() {
        // A populated Codex percentage is shaded by the shared classifier, the
        // same as the usage windows. Isolate the segment so no usage-window
        // color bleeds into the assertion.
        let mut c = cfg();
        c.lines = vec!["{codex}".to_string()];
        let s = snap();
        let cross = CrossProvider {
            codex_five_hour: Some(80.0),
            ..Default::default()
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: Some(&cross),
            config: &c,
            now: now(),
            color: true,
        });
        assert!(out.contains("🌀 5h 80%"), "codex label: {out}");
        // 80% -> Orange = fg:#ff9e64 = rgb(255,158,100).
        assert!(
            out.contains("\x1b[38;2;255;158;100m"),
            "codex 80% must read Orange: {out:?}"
        );
    }

    #[test]
    fn codex_severity_classifies_rounded_value_at_cutoff() {
        // 89.6% shows "◇5h 90%" and must read Red, not Orange - same
        // round-before-classify rule as the usage windows.
        let mut c = cfg();
        c.lines = vec!["{codex}".to_string()];
        let s = snap();
        let cross = CrossProvider {
            codex_five_hour: Some(89.6),
            ..Default::default()
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: Some(&cross),
            config: &c,
            now: now(),
            color: true,
        });
        assert!(out.contains("🌀 5h 90%"), "codex rounds to 90: {out}");
        // Red = bold fg:#f7768e = rgb(247,118,142).
        assert!(
            out.contains("\x1b[1;38;2;247;118;142m"),
            "codex 89.6 shows 90% and must read Red: {out:?}"
        );
    }

    #[test]
    fn codex_renders_both_windows() {
        // Both windows present -> both are rendered, each with its diamond label.
        let mut c = cfg();
        c.lines = vec!["{codex}".to_string()];
        let s = snap();
        let cross = CrossProvider {
            codex_five_hour: Some(2.0),
            codex_weekly: Some(3.0),
            ..Default::default()
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: Some(&cross),
            config: &c,
            now: now(),
            color: false,
        });
        assert!(out.contains("🌀 5h 2%"), "5h window: {out}");
        assert!(out.contains("🌀 7d 3%"), "weekly window labeled 7d: {out}");
    }

    /// A full-config render exercising every segment's glyph at once. This is
    /// the glyph table: provider glyph for rate windows, metric glyph for the
    /// Claude-only figures, exactly one space after each.
    #[test]
    fn glyph_table() {
        let mut c = cfg();
        c.lines = vec!["{model} {context_bar} {cost} {usage} {codex} {openai_cost}".to_string()];
        let s = snap();
        let cross = CrossProvider {
            codex_five_hour: Some(12.0),
            codex_weekly: Some(7.0),
            openai_cost_micro_usd: Some(0),
            ..Default::default()
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: Some(&cross),
            config: &c,
            now: now(),
            color: false,
        });
        assert!(out.contains("🤖 Opus"), "model: {out}");
        // snap() has context 42% and the default bar width is 10 -> 4 filled cells.
        assert!(out.contains("🧠 [####------] 42%"), "context: {out}");
        assert!(out.contains("💰 ~$2.50"), "cost: {out}");
        assert!(out.contains("✳️ 5h 82%"), "claude 5h: {out}");
        assert!(out.contains("✳️ 7d 88%"), "claude 7d: {out}");
        assert!(out.contains("🌀 5h 12%"), "codex 5h: {out}");
        assert!(out.contains("🌀 7d 7%"), "codex weekly labeled 7d: {out}");
        assert!(out.contains("🌀 $0.00"), "openai cost: {out}");
    }

    /// The Codex weekly window must read "7d", never "wk". Every other surface
    /// in the repo already calls it 7d (see balanze_cli/src/render.rs), and the
    /// provider glyph is what distinguishes it from Claude's 7d window.
    #[test]
    fn codex_weekly_is_labeled_7d_not_wk() {
        let mut c = cfg();
        c.lines = vec!["{codex}".to_string()];
        let s = snap();
        let cross = CrossProvider {
            codex_weekly: Some(7.0),
            ..Default::default()
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: Some(&cross),
            config: &c,
            now: now(),
            color: false,
        });
        assert!(out.contains("🌀 7d 7%"), "{out}");
        assert!(!out.contains("wk"), "the 'wk' label is retired: {out}");
    }

    /// The one spacing rule: every glyph is followed by exactly one space, and
    /// the line never contains a double space.
    #[test]
    fn every_glyph_is_followed_by_exactly_one_space() {
        let mut c = cfg();
        c.lines = vec!["{model} {context_bar} {cost} {usage} {codex} {openai_cost}".to_string()];
        let s = snap();
        let cross = CrossProvider {
            codex_five_hour: Some(12.0),
            codex_weekly: Some(7.0),
            openai_cost_micro_usd: Some(0),
            ..Default::default()
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: Some(&cross),
            config: &c,
            now: now(),
            color: false,
        });
        for glyph in ["🤖", "🧠", "💰", "✳️", "🌀"] {
            for (idx, _) in out.match_indices(glyph) {
                let rest = &out[idx + glyph.len()..];
                assert!(
                    rest.starts_with(' '),
                    "glyph {glyph} must be followed by a space: {out:?}"
                );
                assert!(
                    !rest.starts_with("  "),
                    "glyph {glyph} must be followed by exactly one space: {out:?}"
                );
            }
        }
        assert!(!out.contains("  "), "no double spaces anywhere: {out:?}");
    }

    /// The retired glyphs and labels must not survive anywhere in a full render.
    #[test]
    fn retired_glyphs_are_gone() {
        let mut c = cfg();
        c.lines = vec!["{model} {context_bar} {cost} {usage} {codex} {openai_cost}".to_string()];
        let s = snap();
        let cross = CrossProvider {
            codex_five_hour: Some(12.0),
            codex_weekly: Some(7.0),
            openai_cost_micro_usd: Some(0),
            ..Default::default()
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: Some(&cross),
            config: &c,
            now: now(),
            color: false,
        });
        for retired in ['⌛', '📅', '◇'] {
            assert!(!out.contains(retired), "retired glyph {retired}: {out}");
        }
        assert!(
            !out.contains("OpenAI"),
            "the bare 'OpenAI' literal is replaced by the 🌀 glyph: {out}"
        );
    }

    /// The stale marker carries the VS16 variation selector so it advances two
    /// cells like every other glyph. A bare U+26A0 advances one and ragged the
    /// tail of the line.
    #[test]
    fn stale_marker_is_emoji_presentation() {
        let mut c = cfg();
        c.lines = vec!["{codex}".to_string()];
        let s = snap();
        let cross = CrossProvider {
            codex_five_hour: Some(12.0),
            codex_stale: true,
            ..Default::default()
        };
        let out = render(&RenderInput {
            snapshot: &s,
            cross: Some(&cross),
            config: &c,
            now: now(),
            color: false,
        });
        assert!(
            out.ends_with("\u{26A0}\u{FE0F}"),
            "stale marker must be U+26A0 U+FE0F: {out:?}"
        );
    }
}
