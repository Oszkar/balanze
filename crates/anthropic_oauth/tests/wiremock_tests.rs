//! HTTP-layer tests via wiremock. Validates that fetch_usage correctly:
//! - sends Authorization + anthropic-beta headers
//! - reads anthropic-organization-id from response headers
//! - maps HTTP 401 to OAuthError::AuthExpired
//! - maps other non-200 status to OAuthError::UnexpectedStatus
//! - propagates network errors

use anthropic_oauth::{fetch_usage, OAuthError};
use reqwest::Client;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn body_max_user() -> &'static str {
    r#"{
        "five_hour":  {"utilization": 23.0, "resets_at": "2026-05-13T18:00:00+00:00"},
        "seven_day":  {"utilization": 58.0, "resets_at": "2026-05-15T14:00:00+00:00"},
        "seven_day_sonnet": {"utilization": 11.0, "resets_at": "2026-05-15T14:00:00+00:00"},
        "extra_usage": {"is_enabled": true, "monthly_limit": 100.0, "used_credits": 42.5, "utilization": 42.5, "currency": "USD"}
    }"#
}

#[tokio::test]
async fn happy_path_parses_response_and_reads_org_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/oauth/usage"))
        .and(header("Authorization", "Bearer test-token"))
        .and(header("anthropic-beta", "oauth-2025-04-20"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("anthropic-organization-id", "test-org-uuid")
                .set_body_string(body_max_user()),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let snapshot = fetch_usage(
        &client,
        &server.uri(),
        "test-token",
        Some("max".into()),
        Some("default_claude_max_5x".into()),
        &backoff::BackoffPolicy::fail_fast(),
    )
    .await
    .expect("fetch_usage should succeed");

    assert_eq!(snapshot.org_uuid.as_deref(), Some("test-org-uuid"));
    assert_eq!(snapshot.subscription_type.as_deref(), Some("max"));
    assert_eq!(snapshot.cadences.len(), 3);
    assert!(snapshot.extra_usage.is_some());
}

#[tokio::test]
async fn http_401_returns_auth_expired() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/oauth/usage"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"unauthorized"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let err = fetch_usage(
        &client,
        &server.uri(),
        "expired-token",
        None,
        None,
        &backoff::BackoffPolicy::fail_fast(),
    )
    .await
    .expect_err("should fail with 401");
    assert!(matches!(err, OAuthError::AuthExpired), "got {err:?}");
}

#[tokio::test]
async fn http_500_returns_unexpected_status_with_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/oauth/usage"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let err = fetch_usage(
        &client,
        &server.uri(),
        "token",
        None,
        None,
        &backoff::BackoffPolicy::fail_fast(),
    )
    .await
    .expect_err("should fail with 500");
    match err {
        OAuthError::UnexpectedStatus { status, body } => {
            assert_eq!(status, 500);
            assert_eq!(body, "internal error");
        }
        other => panic!("expected UnexpectedStatus, got {other:?}"),
    }
}

#[tokio::test]
async fn invalid_json_body_with_200_returns_response_shape() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/oauth/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let err = fetch_usage(
        &client,
        &server.uri(),
        "token",
        None,
        None,
        &backoff::BackoffPolicy::fail_fast(),
    )
    .await
    .expect_err("should fail on invalid JSON");
    assert!(matches!(err, OAuthError::ResponseShape(_)), "got {err:?}");
}

#[tokio::test]
async fn missing_org_header_is_not_fatal() {
    // The org header is informational. If Anthropic stops sending it for some
    // reason, parsing the response should still succeed; org_uuid is just None.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/oauth/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body_max_user()))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let snapshot = fetch_usage(
        &client,
        &server.uri(),
        "token",
        None,
        None,
        &backoff::BackoffPolicy::fail_fast(),
    )
    .await
    .expect("should succeed without org header");
    assert!(snapshot.org_uuid.is_none());
}
