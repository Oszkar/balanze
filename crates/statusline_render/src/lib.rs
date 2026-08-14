//! Renders the Balanze statusline. Pure: a parsed statusLine snapshot + the
//! `settings::StatuslineConfig` + a clock instant -> the lines Claude Code
//! prints. Threshold coloring, the line-template layout, and the style-string
//! parser live here; the config DATA lives in the `settings` crate.
//!
//! Cross-provider self-compose lives here too. Codex is read locally, while
//! the source adapter supplies an OpenAI cell from the provider-owned gate.

mod render;
mod self_compose;
pub mod style;
pub use render::{CrossProvider, RenderInput, render, template_uses_segment};
pub use self_compose::{CodexWindows, CrossSources, OpenAiCell, self_compose};
