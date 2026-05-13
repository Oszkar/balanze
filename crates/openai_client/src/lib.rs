//! OpenAI Platform API client. v0.1 scope is the credit_grants endpoint —
//! the simplest path to "show me my remaining OpenAI dollars."
//!
//! Endpoint: `GET https://api.openai.com/v1/dashboard/billing/credit_grants`
//! with `Authorization: Bearer <sk-...>`. Returns plain USD doubles
//! (`total_granted`, `total_used`, `total_available`, plus a `grants` array
//! with per-grant amounts and `expires_at` epoch). No cents conversion, no
//! per-model pricing table needed.
//!
//! **Known limitation**: this endpoint returns HTTP 403 for project-scoped
//! keys (`sk-proj-…`). The user must supply a legacy/user API key (`sk-…`).
//! We surface the 403 as a distinct `ForbiddenProjectKey` error variant
//! with hint text so the user knows what to fix.

mod client;
mod types;

pub use client::fetch_credit_grants;
pub use types::{CreditGrants, Grant, OpenAiError};

/// Default base URL for the OpenAI API. Tests override this to point at wiremock.
pub const DEFAULT_API_BASE: &str = "https://api.openai.com";
