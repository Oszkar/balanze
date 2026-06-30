//! Renders the Balanze statusline. Pure: a parsed statusLine snapshot + the
//! `settings::StatuslineConfig` + a clock instant -> the lines Claude Code
//! prints. Threshold coloring, the line-template layout, and the style-string
//! parser live here; the config DATA lives in the `settings` crate.
//!
//! Cross-provider segments (codex, openai_cost) and the per-turn cache arrive
//! in later PRs; their placeholders render empty until then.

mod render;
pub mod style;
pub use render::{CrossProvider, RenderInput, render};
