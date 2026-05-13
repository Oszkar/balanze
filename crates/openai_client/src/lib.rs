//! OpenAI Platform API client.
//!
//! v0.1 calls `GET https://api.openai.com/v1/organization/costs` — the
//! documented Admin API endpoint for monthly spend. Requires an Admin API
//! key (`sk-admin-…`), which only org owners can create at
//! <https://platform.openai.com/settings/organization/admin-keys>.
//!
//! Project keys (`sk-proj-…`) and service account keys do NOT have access
//! to this surface. The legacy `/v1/dashboard/billing/credit_grants`
//! endpoint we used in an earlier draft is being phased out and only
//! worked with legacy user keys, which OpenAI no longer issues.
//!
//! The Costs API returns SPEND over a time range, not a balance. v0.1
//! defaults to "current calendar month" via `costs_this_month`. If callers
//! need a different window they can call `fetch_costs` directly with
//! explicit `start_time` / `end_time`.

mod client;
mod types;

pub use client::{costs_this_month, fetch_costs};
pub use types::{LineItemCost, OpenAiCosts, OpenAiError};

/// Default base URL for the OpenAI API. Tests override this to point at wiremock.
pub const DEFAULT_API_BASE: &str = "https://api.openai.com";
