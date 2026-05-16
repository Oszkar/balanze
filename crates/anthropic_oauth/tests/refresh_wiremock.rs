use anthropic_oauth::{fetch_usage, refresh_access_token, OAuthError};
use std::time::Duration;
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

    let out = refresh_access_token(
        &client,
        &url,
        "client-x",
        "sk-ant-ort01-OLD",
        1_000_000,
        &backoff::BackoffPolicy::fail_fast(),
    )
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

    match refresh_access_token(
        &client,
        &url,
        "client-x",
        "rt",
        0,
        &backoff::BackoffPolicy::fail_fast(),
    )
    .await
    {
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

    match refresh_access_token(
        &client,
        &url,
        "client-x",
        "rt",
        0,
        &backoff::BackoffPolicy::fail_fast(),
    )
    .await
    {
        Err(OAuthError::ResponseShape(msg)) => {
            assert!(
                msg.contains("expires_in"),
                "error message should mention expires_in: {msg}"
            );
        }
        other => panic!("expected ResponseShape, got {other:?}"),
    }
}

#[tokio::test]
async fn fetch_usage_retries_on_429_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/oauth/usage"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/oauth/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let client = reqwest::Client::new();
    let zero = backoff::BackoffPolicy::custom(Duration::ZERO, 2, Duration::ZERO, 3);
    let out = fetch_usage(&client, &server.uri(), "tok", None, None, &zero).await;
    assert!(out.is_ok(), "should succeed after one 429 retry: {out:?}");
}

#[tokio::test]
async fn fetch_usage_401_does_not_retry() {
    let server = MockServer::start().await;
    let mock = Mock::given(method("GET"))
        .and(path("/api/oauth/usage"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .named("oauth 401");
    server.register(mock).await;
    let client = reqwest::Client::new();
    let std_pol = backoff::BackoffPolicy::standard();
    let out = fetch_usage(&client, &server.uri(), "tok", None, None, &std_pol).await;
    assert!(matches!(out, Err(OAuthError::AuthExpired)));
    // server drop verifies .expect(1) — 401 was NOT retried.
}

/// Safety guard: a 5xx response must NOT be retried for the refresh POST.
/// A retry of a token-rotation POST could consume the refresh token on the
/// server side and strand the user into re-`claude login`. Uses the
/// RETRYING `standard()` policy — if the classifier wrongly allowed 5xx
/// retries this would send more than one request and the `.expect(1)` on
/// MockServer drop would fail.
#[tokio::test]
async fn refresh_5xx_does_not_retry() {
    let server = MockServer::start().await;
    let mock = Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .named("refresh 503");
    server.register(mock).await;
    let client = reqwest::Client::new();
    let url = format!("{}/v1/oauth/token", server.uri());

    let out = refresh_access_token(
        &client,
        &url,
        "client-x",
        "sk-ant-ort01-OLD",
        1_000_000,
        &backoff::BackoffPolicy::standard(),
    )
    .await;

    match out {
        Err(OAuthError::RefreshFailed { status, .. }) => {
            assert_eq!(status, 503);
        }
        other => panic!("expected RefreshFailed(503), got {other:?}"),
    }
    // MockServer drop here verifies .expect(1): exactly ONE request was
    // made, proving the dangerous POST was NOT retried under standard().
}

/// Symmetry test: 429 IS the one safe-to-retry class for the refresh POST.
/// Proves the classifier allows the single retry path without risking
/// credential consumption (429 means the server never processed the token
/// exchange).
#[tokio::test]
async fn refresh_retries_on_429_then_succeeds() {
    let server = MockServer::start().await;
    // First call returns 429 (rate-limited, safe to retry).
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // Second call returns 200 with the same success body shape as the
    // existing refresh_success_* test (verbatim reuse, not invented).
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
    // Zero-delay policy so the test is instant; allows ≥1 retry.
    let zero_pol = backoff::BackoffPolicy::custom(Duration::ZERO, 2, Duration::ZERO, 3);

    let out = refresh_access_token(
        &client,
        &url,
        "client-x",
        "sk-ant-ort01-OLD",
        1_000_000,
        &zero_pol,
    )
    .await;

    assert!(
        out.is_ok(),
        "429 should be retried and 200 should succeed: {out:?}"
    );
    let tokens = out.unwrap();
    assert_eq!(tokens.access_token, "sk-ant-oat01-NEWaccess");
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
        &backoff::BackoffPolicy::fail_fast(),
    )
    .await
    .expect("real refresh should succeed");
    assert!(
        out.access_token.starts_with("sk-ant-"),
        "unexpected token shape"
    );
    assert!(out.expires_at_ms > chrono::Utc::now().timestamp_millis());
}
