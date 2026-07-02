use chrono::{DateTime, Duration, Utc};

use crate::style::apply_style;

/// Cross-provider data for the statusline. Populated from the watcher snapshot
/// or self-compose in later PRs; `None` in PR1 (placeholders render empty).
#[derive(Debug, Clone, Default)]
pub struct CrossProvider {
    pub codex_used_percent: Option<f32>,
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
            let text = format!("{} {shown}%", bar(pct, c.width));
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
            let pct = cross.codex_used_percent?;
            let c = &segs.codex;
            let tone = tone_pct(pct, c.warn, c.critical);
            let mark = if cross.codex_stale { " ⚠" } else { "" };
            Some(paint(
                &format!("◇Codex {pct:.0}%{mark}"),
                resolve(&c.style, theme, "codex", Tone::Base),
                resolve(&c.warn_style, theme, "codex", Tone::Warn),
                resolve(&c.critical_style, theme, "codex", Tone::Critical),
                tone,
                input.color,
            ))
        }
        "openai_cost" => {
            let cross = input.cross?;
            let micro = cross.openai_cost_micro_usd?;
            let mark = if cross.openai_stale { " ⚠" } else { "" };
            Some(paint(
                &format!("OpenAI {}{mark}", fmt_money(micro)),
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
        windows.push(render_window("⌛5h", w, Duration::hours(5), c, input));
    }
    if let Some(w) = rl.seven_day() {
        windows.push(render_window("📅7d", w, Duration::days(7), c, input));
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
    let tone = tone_pct(w.used_percent, c.warn, c.critical);
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
    let theme = input.config.theme.as_str();
    paint(
        &text,
        resolve(&c.style, theme, "usage", Tone::Base),
        resolve(&c.warn_style, theme, "usage", Tone::Warn),
        resolve(&c.critical_style, theme, "usage", Tone::Critical),
        tone,
        input.color,
    )
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
            ("context_bar", false) | ("codex", false) => "fg:#7dcfff",
            ("context_bar", true) | ("codex", true) => "fg:#166775",
            // cost, usage, openai_cost, and any unknown segment -> neutral fg.
            (_, false) => "fg:#a9b1d6",
            (_, true) => "fg:#343b58",
        },
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
    fn color_true_wraps_warn_tone_with_warn_style() {
        // 5h at 75% -> warn (>=70, <90); default usage warn_style = "fg:#e0af68"
        // = rgb(224,175,104), no bold.
        let c = cfg();
        let mut s = snap();
        s.rate_limits
            .as_mut()
            .unwrap()
            .windows
            .iter_mut()
            .find(|w| w.key == "five_hour")
            .unwrap()
            .used_percent = 75.0;
        let out = render(&RenderInput {
            snapshot: &s,
            cross: None,
            config: &c,
            now: now(),
            color: true,
        });
        assert!(
            out.contains("\x1b[38;2;224;175;104m"),
            "warn style applied: {out:?}"
        );
    }
}
