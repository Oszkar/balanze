use anthropic_oauth::{refresh_access_token, OAuthError};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn refresh_success_returns_rotated_tokens_and_expiry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "sk-ant-oat01-NEWaccess",
            "refresh_token": "sk-ant-ort01-NEXTrefresh",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;
    let client = reqwest::Client::new();
    let url = format!("{}/v1/oauth/token", server.uri());

    let out = refresh_access_token(&client, &url, "client-x", "sk-ant-ort01-OLD", 1_000_000)
        .await
        .expect("refresh ok");

    assert_eq!(out.access_token, "sk-ant-oat01-NEWaccess");
    assert_eq!(out.refresh_token, "sk-ant-ort01-NEXTrefresh");
    assert_eq!(out.expires_at_ms, 1_000_000 + 3_600 * 1000);
    let dbg = format!("{out:?}");
    assert!(
        !dbg.contains("NEWaccess") && !dbg.contains("NEXTrefresh"),
        "leak: {dbg}"
    );
}

#[tokio::test]
async fn refresh_non_200_is_refresh_failed_with_redacted_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string("bad refresh sk-ant-ort01-LeakedSecretValue0123456789 nope"),
        )
        .mount(&server)
        .await;
    let client = reqwest::Client::new();
    let url = format!("{}/v1/oauth/token", server.uri());

    match refresh_access_token(&client, &url, "client-x", "rt", 0).await {
        Err(OAuthError::RefreshFailed { status, body }) => {
            assert_eq!(status, 400);
            assert!(!body.contains("LeakedSecretValue"), "secret leaked: {body}");
            assert!(body.contains("sk-…REDACTED"), "expected redaction: {body}");
        }
        other => panic!("expected RefreshFailed, got {other:?}"),
    }
}

/// Fix 4 (TDD): non-positive `expires_in` must be rejected — a malformed or
/// hostile response must not yield an already-expired credential.
#[tokio::test]
async fn refresh_zero_expires_in_is_response_shape_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "a",
            "refresh_token": "b",
            "expires_in": 0
        })))
        .mount(&server)
        .await;
    let client = reqwest::Client::new();
    let url = format!("{}/v1/oauth/token", server.uri());

    match refresh_access_token(&client, &url, "client-x", "rt", 0).await {
        Err(OAuthError::ResponseShape(msg)) => {
            assert!(
                msg.contains("expires_in"),
                "error message should mention expires_in: {msg}"
            );
        }
        other => panic!("expected ResponseShape, got {other:?}"),
    }
}

// Real-endpoint smoke. NOT run in CI (no creds there). Maintainer runs:
//   cargo test -p anthropic_oauth -- --ignored refresh_real_endpoint_smoke
// with a valid refresh token exported, before tagging a release. Confirms
// the CLAUDE_CODE_TOKEN_URL / CLAUDE_CODE_CLIENT_ID constants are still good.
#[tokio::test]
#[ignore = "real Anthropic endpoint; run manually with BALANZE_SMOKE_REFRESH_TOKEN set"]
async fn refresh_real_endpoint_smoke() {
    let rt = std::env::var("BALANZE_SMOKE_REFRESH_TOKEN")
        .expect("BALANZE_SMOKE_REFRESH_TOKEN must be set to run this pre-tag release gate");
    let client = reqwest::Client::new();
    let out = refresh_access_token(
        &client,
        anthropic_oauth::CLAUDE_CODE_TOKEN_URL,
        anthropic_oauth::CLAUDE_CODE_CLIENT_ID,
        rt.trim(),
        chrono::Utc::now().timestamp_millis(),
    )
    .await
    .expect("real refresh should succeed");
    assert!(
        out.access_token.starts_with("sk-ant-"),
        "unexpected token shape"
    );
    assert!(out.expires_at_ms > chrono::Utc::now().timestamp_millis());
}
