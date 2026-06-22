//! One-shot validation probe for a user-supplied OpenAI admin key.
//!
//! The settings UI calls this (via the `validate_api_key` Tauri command) so a
//! freshly pasted key gets immediate feedback instead of the user waiting up to
//! a full poll interval to discover a 401/403. It is a single fail-fast
//! month-to-date costs request against the same endpoint the poller uses, so a
//! key that validates here will work for the real poll.
//!
//! Classification lives here, not at the IPC boundary, because this crate
//! already owns the permanent-vs-transient distinction for the poller's backoff
//! (see `openai_poll`): a 401/403 is a definitively wrong key; a network error
//! or 429 is transient and the caller may choose to store the key anyway and
//! let the poller retry.

use openai_client::OpenAiError;

/// Result of probing a key. `Valid` means it authenticated; `Rejected` means
/// the key is definitively wrong (do not store it); `Unreachable` means the
/// check failed transiently (network / rate limit) and the caller may store the
/// key anyway and let the poller retry. The string is user-facing copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyProbe {
    Valid,
    Rejected(String),
    Unreachable(String),
}

/// Probe an OpenAI admin key without storing it. Never logs or echoes the key.
pub async fn validate_openai_key(key: &str) -> KeyProbe {
    let client = match reqwest::Client::builder()
        .user_agent("balanze-validate/0.1.0")
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return KeyProbe::Unreachable(format!(
                "Could not start the validation request ({e}). You can save the key and Balanze will retry."
            ));
        }
    };
    match openai_client::costs_this_month(
        &client,
        openai_client::DEFAULT_API_BASE,
        key,
        &backoff::BackoffPolicy::fail_fast(),
    )
    .await
    {
        Ok(_) => KeyProbe::Valid,
        Err(e) => classify(&e),
    }
}

/// Map a probe error to an outcome + user-facing message. Pure (no I/O) so the
/// permanent-vs-transient rules are unit-testable without the network.
fn classify(err: &OpenAiError) -> KeyProbe {
    match err {
        OpenAiError::AuthInvalid { .. } => KeyProbe::Rejected(
            "OpenAI rejected this key (HTTP 401 - invalid or revoked). Paste a current admin key (sk-admin-...)."
                .to_string(),
        ),
        OpenAiError::InsufficientScope { .. } => KeyProbe::Rejected(
            "This key lacks admin scope (HTTP 403). Reading organization costs needs an admin key (sk-admin-...), not a project or service-account key. Create one at https://platform.openai.com/settings/organization/admin-keys."
                .to_string(),
        ),
        OpenAiError::RateLimited { .. } => KeyProbe::Unreachable(
            "OpenAI is rate-limiting right now, so the key could not be checked. You can save it and Balanze will retry."
                .to_string(),
        ),
        OpenAiError::Network(_) => KeyProbe::Unreachable(
            "Could not reach OpenAI to check the key (network error). You can save it and Balanze will retry."
                .to_string(),
        ),
        OpenAiError::UnexpectedStatus { status, .. } => KeyProbe::Unreachable(format!(
            "OpenAI returned an unexpected status ({status}) while checking the key. You can save it and Balanze will retry."
        )),
        OpenAiError::ResponseShape(_) => KeyProbe::Unreachable(
            "OpenAI's response was not in the expected shape while checking the key. You can save it and Balanze will retry."
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_invalid_is_rejected_definitively() {
        // A 401 means the key is wrong - the UI must not store it.
        assert!(matches!(
            classify(&OpenAiError::AuthInvalid { body: "{}".into() }),
            KeyProbe::Rejected(_)
        ));
    }

    #[test]
    fn insufficient_scope_is_rejected_and_links_to_admin_keys() {
        // A 403 (project/service-account key) is also definitive; the message
        // must point the user at where to mint an admin key.
        match classify(&OpenAiError::InsufficientScope { body: "{}".into() }) {
            KeyProbe::Rejected(msg) => {
                assert!(
                    msg.contains("admin-keys"),
                    "msg should link admin-keys: {msg}"
                );
                assert!(
                    msg.contains("sk-admin-"),
                    "msg should name the key shape: {msg}"
                );
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn rate_limited_is_transient_save_anyway() {
        // 429 is transient: the key might be fine, so the caller may store it.
        assert!(matches!(
            classify(&OpenAiError::RateLimited { retry_after: None }),
            KeyProbe::Unreachable(_)
        ));
    }

    #[test]
    fn unexpected_status_is_transient_and_names_the_status() {
        match classify(&OpenAiError::UnexpectedStatus {
            status: 503,
            body: "down".into(),
        }) {
            KeyProbe::Unreachable(msg) => assert!(msg.contains("503"), "msg: {msg}"),
            other => panic!("expected Unreachable, got {other:?}"),
        }
    }

    #[test]
    fn response_shape_is_transient() {
        assert!(matches!(
            classify(&OpenAiError::ResponseShape("bad json".into())),
            KeyProbe::Unreachable(_)
        ));
    }
}
