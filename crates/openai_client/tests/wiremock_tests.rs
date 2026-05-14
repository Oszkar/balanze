//! HTTP-layer tests via wiremock. Validates that fetch_costs:
//! - sends Authorization Bearer header
//! - sends start_time / end_time / bucket_width / group_by query params
//! - maps 401 → AuthInvalid
//! - maps 403 → InsufficientScope (with hint message intact)
//! - maps other non-200 → UnexpectedStatus

use chrono::{TimeZone, Utc};
use openai_client::{fetch_costs, OpenAiError};
use reqwest::Client;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn body_typical() -> &'static str {
    r#"{
        "object": "page",
        "data": [
            {
                "object": "bucket",
                "start_time": 1746057600,
                "end_time": 1746144000,
                "results": [
                    {"object":"organization.costs.result","amount":{"value":1.50,"currency":"usd"},"line_item":"gpt-5"},
                    {"object":"organization.costs.result","amount":{"value":0.23,"currency":"usd"},"line_item":"o1-mini"}
                ]
            }
        ],
        "has_more": false
    }"#
}

fn window() -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>) {
    let start = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).single().unwrap();
    let end = Utc
        .with_ymd_and_hms(2026, 5, 13, 10, 0, 0)
        .single()
        .unwrap();
    (start, end)
}

#[tokio::test]
async fn happy_path_parses_response_and_sends_expected_query() {
    let server = MockServer::start().await;
    let (start, end) = window();
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .and(header("Authorization", "Bearer sk-admin-test"))
        .and(query_param("start_time", start.timestamp().to_string()))
        .and(query_param("end_time", end.timestamp().to_string()))
        .and(query_param("bucket_width", "1d"))
        .and(query_param("group_by[]", "line_item"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body_typical()))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let costs = fetch_costs(&client, &server.uri(), "sk-admin-test", start, Some(end))
        .await
        .expect("should succeed");

    assert!((costs.total_usd - 1.73).abs() < 1e-9);
    assert_eq!(costs.by_line_item.len(), 2);
    assert!(!costs.truncated);
    // gpt-5 has the higher amount, comes first.
    assert_eq!(costs.by_line_item[0].line_item, "gpt-5");
}

#[tokio::test]
async fn http_401_returns_auth_invalid() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string(r#"{"error":{"message":"bad key"}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let (start, end) = window();
    let err = fetch_costs(&client, &server.uri(), "sk-admin-bad", start, Some(end))
        .await
        .expect_err("should fail with 401");
    assert!(
        matches!(err, OpenAiError::AuthInvalid { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn http_403_returns_insufficient_scope_with_hint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(
            ResponseTemplate::new(403).set_body_string(r#"{"error":{"message":"forbidden"}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let (start, end) = window();
    let err = fetch_costs(&client, &server.uri(), "sk-proj-XYZ", start, Some(end))
        .await
        .expect_err("should fail with 403");
    let displayed = format!("{err}");
    match &err {
        OpenAiError::InsufficientScope { body } => {
            assert!(body.contains("forbidden"));
            assert!(displayed.contains("admin"), "hint missing: {displayed}");
            assert!(
                displayed.contains("admin-keys"),
                "URL hint missing: {displayed}"
            );
        }
        other => panic!("expected InsufficientScope, got {other:?}"),
    }
}

#[tokio::test]
async fn http_500_returns_unexpected_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal server error"))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let (start, end) = window();
    let err = fetch_costs(&client, &server.uri(), "sk-admin-test", start, Some(end))
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
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let (start, end) = window();
    let err = fetch_costs(&client, &server.uri(), "sk-admin-test", start, Some(end))
        .await
        .expect_err("should fail on invalid JSON");
    assert!(matches!(err, OpenAiError::ResponseShape(_)), "got {err:?}");
}
