//! Authoritative Claude subscription state via Anthropic's OAuth usage endpoint.
//!
//! Reads `~/.claude/.credentials.json` (with fallback to `~/.config/claude/.credentials.json`
//! per AGENTS.md §2.1), calls `GET https://api.anthropic.com/api/oauth/usage` with the
//! bearer token, and parses the response into a `ClaudeOAuthSnapshot`.
//!
//! **Secret discipline**: this is the only crate in the workspace that reads the
//! credentials file. The access token, refresh token, and every other field under
//! `claudeAiOauth` are treated as secrets — never logged, never echoed, never
//! persisted by Balanze. See AGENTS.md §3.4.

mod client;
mod credentials;
mod refresh;
mod types;

pub use client::fetch_usage;
pub use credentials::{load, load_from, locate_credentials, write_back, WriteBack};
pub use refresh::{refresh_access_token, CLAUDE_CODE_CLIENT_ID, CLAUDE_CODE_TOKEN_URL};
pub use types::{
    CadenceBar, ClaudeOAuthSnapshot, Credentials, CredentialsClaudeAiOauth, ExtraUsage, OAuthError,
    RefreshedTokens,
};

/// Default base URL for Anthropic's API. Tests override this to point at wiremock.
pub const DEFAULT_API_BASE: &str = "https://api.anthropic.com";
