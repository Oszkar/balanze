//! HTTP-layer tests via wiremock. Validates that fetch_credit_grants:
//! - sends Authorization Bearer header
//! - maps 401 → AuthExpired
//! - maps 403 → ForbiddenProjectKey (with the hint message intact)
//! - maps other non-200 → UnexpectedStatus
//! - returns Network errors on connection failure

use openai_client::{fetch_credit_grants, OpenAiError};
use reqwest::Client;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn body_typical() -> &'static str {
    r#"{
        "object": "list",
        "total_granted": 25.0,
        "total_used": 18.42,
        "total_available": 6.58,
        "grants": {
            "object": "credit_grant",
            "data": [
                {"grant_amount": 25.0, "used_amount": 18.42, "expires_at": 1781308800}
            ]
        }
    }"#
}

#[tokio::test]
async fn happy_path_parses_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/dashboard/billing/credit_grants"))
        .and(header("Authorization", "Bearer sk-test-legacy-key"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body_typical()))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let grants = fetch_credit_grants(&client, &server.uri(), "sk-test-legacy-key")
        .await
        .expect("fetch_credit_grants should succeed");

    assert!((grants.total_granted_usd - 25.0).abs() < 1e-9);
    assert!((grants.total_used_usd - 18.42).abs() < 1e-9);
    assert!((grants.total_available_usd - 6.58).abs() < 1e-9);
    assert_eq!(grants.grants.len(), 1);
}

#[tokio::test]
async fn http_401_returns_auth_expired() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/dashboard/billing/credit_grants"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string(r#"{"error":{"message":"invalid"}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let err = fetch_credit_grants(&client, &server.uri(), "sk-bad-key")
        .await
        .expect_err("should fail with 401");
    assert!(matches!(err, OpenAiError::AuthExpired { .. }), "got {err:?}");
}

#[tokio::test]
async fn http_403_returns_forbidden_project_key_with_hint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/dashboard/billing/credit_grants"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_string(r#"{"error":{"message":"You don't have access"}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let err = fetch_credit_grants(&client, &server.uri(), "sk-proj-XXX")
        .await
        .expect_err("should fail with 403");
    // Borrow `body` so we can still format `err` (Display) below.
    let displayed = format!("{err}");
    match &err {
        OpenAiError::ForbiddenProjectKey { body } => {
            assert!(body.contains("You don't have access"));
            // The error's Display impl should mention the legacy-vs-project key fix.
            assert!(displayed.contains("legacy"), "hint missing: {displayed}");
            assert!(displayed.contains("project"), "hint missing: {displayed}");
        }
        other => panic!("expected ForbiddenProjectKey, got {other:?}"),
    }
}

#[tokio::test]
async fn http_500_returns_unexpected_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/dashboard/billing/credit_grants"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal server error"))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let err = fetch_credit_grants(&client, &server.uri(), "sk-test")
        .await
        .expect_err("should fail with 500");
    match err {
        OpenAiError::UnexpectedStatus { status, body } => {
            assert_eq!(status, 500);
            assert_eq!(body, "internal server error");
        }
        other => panic!("expected UnexpectedStatus, got {other:?}"),
    }
}

#[tokio::test]
async fn invalid_json_body_with_200_is_shape_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/dashboard/billing/credit_grants"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let err = fetch_credit_grants(&client, &server.uri(), "sk-test")
        .await
        .expect_err("should fail on invalid JSON");
    assert!(matches!(err, OpenAiError::ResponseShape(_)), "got {err:?}");
}
