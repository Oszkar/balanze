//! Authoritative Claude subscription state via Anthropic's OAuth usage endpoint.
//!
//! Reads Claude Code's OAuth credential (a `.credentials.json` file, or on
//! macOS the login Keychain entry when no file exists — see `credentials`),
//! calls `GET https://api.anthropic.com/api/oauth/usage` with the bearer token,
//! and parses the response into a `ClaudeOAuthSnapshot`.
//!
//! **Secret discipline**: this is the only crate in the workspace that reads the
//! credential. The access token, refresh token, and every other field under
//! `claudeAiOauth` are treated as secrets — never logged, never echoed, never
//! persisted by Balanze. Balanze refreshes + writes back only a file source it
//! owns; the macOS Keychain entry is Claude Code's and is read-only. See
//! AGENTS.md §3.4.

mod client;
mod credentials;
mod refresh;
mod types;

pub use client::fetch_usage;
pub use credentials::{
    CredentialSource, WriteBack, load, load_from, load_from_source, locate_credentials, write_back,
};
pub use refresh::{CLAUDE_CODE_CLIENT_ID, CLAUDE_CODE_TOKEN_URL, refresh_access_token};
pub use types::{
    CadenceBar, ClaudeOAuthSnapshot, Credentials, CredentialsClaudeAiOauth, ExtraUsage, OAuthError,
    RefreshedTokens,
};

/// Default base URL for Anthropic's API. Tests override this to point at wiremock.
pub const DEFAULT_API_BASE: &str = "https://api.anthropic.com";
