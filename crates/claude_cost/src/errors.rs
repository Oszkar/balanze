use thiserror::Error;

/// Errors surfaced by `claude_cost`.
///
/// Only the parser path is fallible. `compute_cost` is infallible by
/// design: unknown models route into `Cost::skipped_models`, never an
/// error variant. See AGENTS.md §3.2 for the error-handling discipline.
#[derive(Debug, Error)]
pub enum CostError {
    /// The bundled price table failed to parse, or required fields were
    /// missing.
    ///
    /// Compile-time impossible for the vendored data shipped in this repo
    /// (`build.rs` gates filename presence; `load_bundled_prices` then
    /// parses a JSON that's known-good at vendoring time). The variant
    /// exists because the parser helper is also exercised on synthetic
    /// input in unit tests, and to leave a useful error path if a
    /// downstream refresh ever produces an unparseable file.
    #[error("bundled price table failed to parse: {0}")]
    PricesMissing(String),
}
