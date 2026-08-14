//! OpenAI Platform API client.
//!
//! Calls `GET https://api.openai.com/v1/organization/costs` - the
//! documented Admin API endpoint for monthly spend. Requires an Admin API
//! key (`sk-admin-...`), which only org owners can create at
//! <https://platform.openai.com/settings/organization/admin-keys>.
//!
//! Project keys (`sk-proj-...`) and service account keys do NOT have access
//! to this surface. The legacy `/v1/dashboard/billing/credit_grants`
//! endpoint we used in an earlier draft is being phased out and only
//! worked with legacy user keys, which OpenAI no longer issues.
//!
//! The Costs API returns SPEND over a time range, not a balance. Balanze
//! exposes one month-to-date operation through a durable, process-wide
//! 300-second request gate. Raw Costs HTTP functions remain private so every
//! production caller shares the same reservation invariant.

mod client;
mod gate;
mod types;

pub use gate::{
    COSTS_GATE_SECS, MAX_GATE_IDENTITIES, cache_dir_path, gated_costs_this_month,
    gated_costs_this_month_with_cache,
};
pub use types::{
    CachedOpenAiCosts, CostsGateError, GateDeferredReason, LineItemCost, OpenAiCosts, OpenAiError,
    StoredFailureKind,
};

/// Default base URL for the OpenAI API. Tests override this to point at wiremock.
pub const DEFAULT_API_BASE: &str = "https://api.openai.com";

/// Resolve the Costs API base once per caller. The override is a test seam;
/// production uses [`DEFAULT_API_BASE`].
pub fn api_base_url() -> String {
    std::env::var("BALANZE_OPENAI_API_BASE").unwrap_or_else(|_| DEFAULT_API_BASE.to_string())
}
