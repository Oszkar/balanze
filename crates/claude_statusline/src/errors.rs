use thiserror::Error;

/// Errors from parsing the statusLine payload OR managing the statusLine
/// stanza in Claude Code's settings.json. One enum (mirrors
/// `anthropic_oauth::OAuthError`'s single-enum approach).
#[derive(Debug, Error)]
pub enum StatuslineError {
    #[error("statusline payload is not valid JSON: {0}")]
    InvalidJson(String),

    #[error("statusline payload schema drift: {message}")]
    SchemaDrift { message: String },

    #[error("Claude settings.json not found (looked at {searched:?})")]
    SettingsMissing { searched: Vec<std::path::PathBuf> },

    #[error("io error on {path:?}: {source}")]
    SettingsIo {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Claude settings.json at {path:?} is malformed: {reason}")]
    SettingsMalformed {
        path: std::path::PathBuf,
        reason: String,
    },
}
