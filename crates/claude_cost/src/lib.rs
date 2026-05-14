//! Pure-function crate that synthesizes estimated Claude API cost from
//! [`claude_parser::UsageEvent`] slices.
//!
//! Per AGENTS.md §4 boundary #2, this crate is I/O-free on the hot path
//! and synchronous. No `tokio::spawn`, no `reqwest`, no logging above
//! `debug`. The only I/O is [`load_bundled_prices`], which uses
//! `include_str!` to pull the vendored LiteLLM Anthropic-subset snapshot
//! at compile time.
//!
//! # Cost discipline
//!
//! AGENTS.md §2.1 mandates `i64` micro-USD for currency totals. This
//! crate stores per-token prices in nano-USD (`1e-9` USD) internally to
//! avoid precision loss on cache_read prices (~0.3 micro-USD/token
//! would round to 0). All intermediate multiplication uses `i128` to
//! avoid overflow on large token counts. Outputs are converted to `i64`
//! micro-USD at the boundary of the [`Cost`] and [`ModelCost`] structs,
//! saturating at `i64::MAX` / `i64::MIN` rather than panicking.
//!
//! # Provenance
//!
//! The bundled price table is sourced from BerriAI/litellm's
//! `model_prices_and_context_window.json`, filtered to bare `claude-*`
//! keys. Provenance is exposed via [`PRICE_TABLE_COMMIT`] and
//! [`PRICE_TABLE_DATE`] consts, emitted at build time by `build.rs`
//! parsing the data filename. See `data/LICENSE-LITELLM` for MIT
//! attribution and the crate `README.md` for the refresh procedure.

pub mod compute;
pub mod errors;
pub mod prices;

pub use compute::{compute_cost, Cost, ModelCost};
pub use errors::CostError;
pub use prices::{load_bundled_prices, ModelPrices, PriceTable};

/// Short commit hash of the LiteLLM source the vendored price table was
/// taken from. Emitted by `build.rs` from the `data/litellm-prices-*.json`
/// filename so the const and the data file can never drift.
pub const PRICE_TABLE_COMMIT: &str = env!("PRICE_TABLE_COMMIT");

/// Vendoring date of the bundled price table as `YYYY-MM-DD`. Emitted by
/// `build.rs` from the data filename.
pub const PRICE_TABLE_DATE: &str = env!("PRICE_TABLE_DATE");
