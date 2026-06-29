use chrono::{DateTime, Duration, Utc};

use crate::style::apply_style;

/// Cross-provider data for the statusline. Populated from the watcher snapshot
/// or self-compose in later PRs; `None` in PR1 (placeholders render empty).
#[derive(Debug, Clone, Default)]
pub struct CrossProvider {
    pub codex_used_percent: Option<f32>,
    pub openai_cost_micro_usd: Option<i64>,
    /// True when this cross-provider data is stale (drives the staleness mark).
    pub stale: bool,
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
    match key {
        "model" => {
            let name = snap.model_display_name.as_deref()?;
            Some(paint(
                &format!("🤖 {name}"),
                &segs.model.style,
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
            let text = format!("{} {:.0}%", bar(pct, c.width), pct);
            Some(paint(
                &text,
                &c.style,
                &c.warn_style,
                &c.critical_style,
                tone,
                input.color,
            ))
        }
        "cost" => {
            let micro = snap.session_cost_micro_usd?;
            let c = &segs.cost;
            let tone = tone_money(micro, c.warn_micro_usd, c.critical_micro_usd);
            Some(paint(
                &format!("💰 {}", fmt_money(micro)),
                &c.style,
                &c.warn_style,
                &c.critical_style,
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
            let mark = if cross.stale { " ⚠" } else { "" };
            Some(paint(
                &format!("◇Codex {pct:.0}%{mark}"),
                &c.style,
                &c.warn_style,
                &c.critical_style,
                tone,
                input.color,
            ))
        }
        "openai_cost" => {
            let cross = input.cross?;
            let micro = cross.openai_cost_micro_usd?;
            let mark = if cross.stale { " ⚠" } else { "" };
            Some(paint(
                &format!("OpenAI {}{mark}", fmt_money(micro)),
                &segs.openai_cost.style,
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
    if let Some(w) = &rl.five_hour {
        windows.push(render_window("⌛5h", w, Duration::hours(5), c, input));
    }
    if let Some(w) = &rl.seven_day {
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
    let mut text = format!("{label} {:.0}%", w.used_percent);
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
    paint(
        &text,
        &c.style,
        &c.warn_style,
        &c.critical_style,
        tone,
        input.color,
    )
}

/// Style seam. A LATER task replaces the body with real ANSI application; here
/// it returns the text unchanged so the layout/text can be tested in isolation.
fn paint(text: &str, base: &str, warn: &str, crit: &str, tone: Tone, color: bool) -> String {
    let _ = (
        base,
        warn,
        crit,
        tone,
        color,
        apply_style as fn(&str, &str) -> String,
    );
    text.to_string()
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
                five_hour: Some(claude_statusline::RateWindow {
                    used_percent: 82.0,
                    resets_at: now() + chrono::Duration::minutes(83),
                }),
                seven_day: Some(claude_statusline::RateWindow {
                    used_percent: 88.0,
                    resets_at: now() + chrono::Duration::days(5),
                }),
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
        assert!(out.contains("💰 $2.50"), "cost: {out}");
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
}
