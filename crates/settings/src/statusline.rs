//! Statusline display configuration: theme, line templates, and per-segment
//! styles + thresholds. Lives in `settings` because it is config DATA; the
//! rendering behavior that consumes it lives in the `statusline_render` crate.
//!
//! Thresholds use integer percent (`u32`) and i64 micro-USD (AGENTS.md §2.1:
//! never f64 for threshold comparisons), which also keeps `Settings: Eq`.
//! Threshold/width/flag defaults match the maintainer's cship taste so a switch
//! is visually lossless; everything is overridable.
//!
//! Per-segment STYLE defaults are intentionally EMPTY: an empty style resolves
//! to the `theme` palette in the `statusline_render` crate (dark or light). This
//! is what makes `theme` actually switch colors and what lets a partial override
//! (changing only a width or threshold) keep its coloring instead of going
//! colorless. An explicit non-empty style overrides the theme palette for the
//! `model`, `context_bar`, `cost`, and `openai_cost` segments.
//!
//! Exception - `usage` and `codex`: since the cross-surface color unification
//! these two are shaded by the shared `window::Severity` classifier (50/75/90),
//! so their `*_style` overrides are currently INERT. The fields are retained as
//! the hook for the future user-configurable per-band styling work, which needs
//! green/orange slots the 3-way base/warn/critical shape lacks. See
//! `UsageSegment` / `PctSegment`.

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatuslineConfig {
    /// Palette for segments whose style is left blank.
    /// Accepted: "dark" | "light". Unrecognized values fall back to the dark
    /// palette in the renderer.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Line templates: each is a space-separated layout of `{segment}`
    /// placeholders (model, agent, context_bar, cost, usage, codex,
    /// openai_cost). Empty segments are dropped; literal text is kept.
    /// `openai_cost` is available but absent from the default lines - see
    /// `default_lines`.
    #[serde(default = "default_lines")]
    pub lines: Vec<String>,
    #[serde(default)]
    pub segments: SegmentConfigs,
    /// The foreign `statusLine.command` displaced by a Balanze "replace", kept
    /// so `balanze-cli statusline restore` / the Settings "Restore" control can
    /// put it back. `None` when Balanze has not replaced anything.
    #[serde(default)]
    pub replaced_command: Option<String>,
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_lines() -> Vec<String> {
    vec![
        "{model} {agent}".to_string(),
        // `openai_cost` is deliberately absent: it is an uncapped dollar figure
        // with no rolling window, so it does not read against a line that is
        // otherwise percent-of-window. It stays implemented and configurable.
        "{context_bar} {cost} {usage} {codex}".to_string(),
    ]
}

impl Default for StatuslineConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            lines: default_lines(),
            segments: SegmentConfigs::default(),
            replaced_command: None,
        }
    }
}

/// Per-segment config. Style-only segments (model/agent/openai_cost) default to
/// empty styles (themed by the renderer); the threshold-bearing segments carry
/// curated thresholds via their own `Default`. The curated colors live in
/// `statusline_render`'s palette, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SegmentConfigs {
    #[serde(default)]
    pub model: StyleOnly,
    #[serde(default)]
    pub agent: StyleOnly,
    #[serde(default)]
    pub context_bar: ContextSegment,
    #[serde(default)]
    pub cost: MoneySegment,
    #[serde(default)]
    pub usage: UsageSegment,
    #[serde(default)]
    pub codex: PctSegment,
    #[serde(default)]
    pub openai_cost: StyleOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StyleOnly {
    #[serde(default)]
    pub style: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSegment {
    /// `warn` must be <= `critical` for the warn band to be reachable.
    #[serde(default = "default_context_warn")]
    pub warn: u32,
    #[serde(default = "default_context_critical")]
    pub critical: u32,
    #[serde(default = "default_bar_width")]
    pub width: u32,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub warn_style: String,
    #[serde(default)]
    pub critical_style: String,
}

fn default_bar_width() -> u32 {
    10
}

fn default_context_warn() -> u32 {
    40
}

fn default_context_critical() -> u32 {
    70
}

impl Default for ContextSegment {
    fn default() -> Self {
        Self {
            warn: default_context_warn(),
            critical: default_context_critical(),
            width: default_bar_width(),
            style: String::new(),
            warn_style: String::new(),
            critical_style: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoneySegment {
    /// Warn at/above this many micro-USD (i64 per §2.1).
    /// `warn_micro_usd` must be <= `critical_micro_usd` for the warn band to be
    /// reachable.
    #[serde(default = "default_cost_warn_micro_usd")]
    pub warn_micro_usd: i64,
    #[serde(default = "default_cost_critical_micro_usd")]
    pub critical_micro_usd: i64,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub warn_style: String,
    #[serde(default)]
    pub critical_style: String,
}

fn default_cost_warn_micro_usd() -> i64 {
    2_000_000
}

fn default_cost_critical_micro_usd() -> i64 {
    5_000_000
}

impl Default for MoneySegment {
    fn default() -> Self {
        Self {
            warn_micro_usd: default_cost_warn_micro_usd(),
            critical_micro_usd: default_cost_critical_micro_usd(),
            style: String::new(),
            warn_style: String::new(),
            critical_style: String::new(),
        }
    }
}

/// Usage-window (5h / 7d) segment config. NOTE: since the cross-surface color
/// unification, the window COLOR is driven by the shared `window::Severity`
/// classifier (50 / 75 / 90), not by `warn`/`critical` here - those and the
/// `*_style` fields are retained as the hook for future user-configurable
/// per-segment thresholds. `show_pace` / `show_reset` are still honored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSegment {
    /// `warn` must be <= `critical` for the warn band to be reachable.
    #[serde(default = "default_usage_warn")]
    pub warn: u32,
    #[serde(default = "default_usage_critical")]
    pub critical: u32,
    #[serde(default = "default_true")]
    pub show_pace: bool,
    #[serde(default = "default_true")]
    pub show_reset: bool,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub warn_style: String,
    #[serde(default)]
    pub critical_style: String,
}

fn default_usage_warn() -> u32 {
    70
}

fn default_usage_critical() -> u32 {
    90
}

impl Default for UsageSegment {
    fn default() -> Self {
        Self {
            warn: default_usage_warn(),
            critical: default_usage_critical(),
            show_pace: true,
            show_reset: true,
            style: String::new(),
            warn_style: String::new(),
            critical_style: String::new(),
        }
    }
}

/// Codex quota segment config. Like `UsageSegment`, the COLOR now comes from
/// the shared `window::Severity` classifier (50 / 75 / 90); `warn`/`critical`
/// and the `*_style` fields are retained for future configurable thresholds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PctSegment {
    /// `warn` must be <= `critical` for the warn band to be reachable.
    #[serde(default = "default_pct_warn")]
    pub warn: u32,
    #[serde(default = "default_pct_critical")]
    pub critical: u32,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub warn_style: String,
    #[serde(default)]
    pub critical_style: String,
}

fn default_pct_warn() -> u32 {
    70
}

fn default_pct_critical() -> u32 {
    90
}

impl Default for PctSegment {
    fn default() -> Self {
        Self {
            warn: default_pct_warn(),
            critical: default_pct_critical(),
            style: String::new(),
            warn_style: String::new(),
            critical_style: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaced_command_defaults_to_none() {
        let c: StatuslineConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c.replaced_command, None);
    }

    #[test]
    fn replaced_command_round_trips() {
        let c = StatuslineConfig {
            replaced_command: Some("cship".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: StatuslineConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.replaced_command, Some("cship".to_string()));
    }

    /// OpenAI API spend is an uncapped dollar figure with no rolling window, so
    /// it does not belong on a default line that is otherwise percent-of-window.
    /// The segment stays implemented and configurable; it is just off by default.
    #[test]
    fn default_lines_omit_the_openai_cost_segment() {
        let lines = default_lines();
        assert!(
            !lines.iter().any(|l| l.contains("{openai_cost}")),
            "openai_cost must be off by default: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("{codex}")),
            "the Codex windows stay on the default line: {lines:?}"
        );
    }
}
