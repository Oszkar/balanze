//! Statusline display configuration: theme, line templates, and per-segment
//! styles + thresholds. Lives in `settings` because it is config DATA; the
//! rendering behavior that consumes it lives in the `statusline_render` crate.
//!
//! Thresholds use integer percent (`u32`) and i64 micro-USD (AGENTS.md §2.1:
//! never f64 for threshold comparisons), which also keeps `Settings: Eq`.
//! Default values match the maintainer's cship taste so a switch is visually
//! lossless; everything is overridable.

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatuslineConfig {
    /// "dark" | "light": palette for segments whose style is left blank.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Line templates: each is a space-separated layout of `{segment}`
    /// placeholders (model, agent, context_bar, cost, usage, codex,
    /// openai_cost). Empty segments are dropped; literal text is kept.
    #[serde(default = "default_lines")]
    pub lines: Vec<String>,
    #[serde(default)]
    pub segments: SegmentConfigs,
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_lines() -> Vec<String> {
    vec![
        "{model} {agent}".to_string(),
        "{context_bar} {cost} {usage} {codex} {openai_cost}".to_string(),
    ]
}

impl Default for StatuslineConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            lines: default_lines(),
            segments: SegmentConfigs::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

impl Default for SegmentConfigs {
    fn default() -> Self {
        Self {
            model: StyleOnly {
                style: "bold fg:#7aa2f7".to_string(),
            },
            agent: StyleOnly {
                style: "fg:#9ece6a".to_string(),
            },
            context_bar: ContextSegment::default(),
            cost: MoneySegment::default(),
            usage: UsageSegment::default(),
            codex: PctSegment {
                warn: 70,
                critical: 90,
                style: "fg:#7dcfff".to_string(),
                warn_style: "fg:#e0af68".to_string(),
                critical_style: "bold fg:#f7768e".to_string(),
            },
            openai_cost: StyleOnly {
                style: "fg:#a9b1d6".to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StyleOnly {
    #[serde(default)]
    pub style: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSegment {
    #[serde(default)]
    pub warn: u32,
    #[serde(default)]
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

impl Default for ContextSegment {
    fn default() -> Self {
        Self {
            warn: 40,
            critical: 70,
            width: default_bar_width(),
            style: "fg:#7dcfff".to_string(),
            warn_style: "fg:#e0af68".to_string(),
            critical_style: "bold fg:#f7768e".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoneySegment {
    /// Warn at/above this many micro-USD (i64 per §2.1).
    #[serde(default)]
    pub warn_micro_usd: i64,
    #[serde(default)]
    pub critical_micro_usd: i64,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub warn_style: String,
    #[serde(default)]
    pub critical_style: String,
}

impl Default for MoneySegment {
    fn default() -> Self {
        Self {
            warn_micro_usd: 2_000_000,
            critical_micro_usd: 5_000_000,
            style: "fg:#a9b1d6".to_string(),
            warn_style: "fg:#e0af68".to_string(),
            critical_style: "bold fg:#f7768e".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSegment {
    #[serde(default)]
    pub warn: u32,
    #[serde(default)]
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

impl Default for UsageSegment {
    fn default() -> Self {
        Self {
            warn: 70,
            critical: 90,
            show_pace: true,
            show_reset: true,
            style: "fg:#a9b1d6".to_string(),
            warn_style: "fg:#e0af68".to_string(),
            critical_style: "bold fg:#f7768e".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PctSegment {
    #[serde(default)]
    pub warn: u32,
    #[serde(default)]
    pub critical: u32,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub warn_style: String,
    #[serde(default)]
    pub critical_style: String,
}

impl Default for PctSegment {
    fn default() -> Self {
        Self {
            warn: 70,
            critical: 90,
            style: "fg:#7dcfff".to_string(),
            warn_style: "fg:#e0af68".to_string(),
            critical_style: "bold fg:#f7768e".to_string(),
        }
    }
}
