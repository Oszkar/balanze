//! Public API tests for the provider-owned OpenAI Costs gate.

use std::time::Duration;

use openai_client::{CostsGateError, StoredFailureKind, gated_costs_this_month_with_cache};
use tempfile::tempdir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn body_typical() -> &'static str {
    r#"{
        "object": "page",
        "data": [{
            "object": "bucket",
            "results": [
                {"amount":{"value":1.50,"currency":"usd"},"line_item":"gpt-5"},
                {"amount":{"value":0.23,"currency":"usd"},"line_item":"o1-mini"}
            ]
        }],
        "has_more": false
    }"#
}

#[tokio::test]
async fn sequential_successes_share_one_full_result_and_one_get() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .and(header("Authorization", "Bearer test-admin-key"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body_typical()))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempdir().unwrap();

    let first = gated_costs_this_month_with_cache(
        &server.uri(),
        "test-admin-key",
        Duration::from_secs(30),
        dir.path().to_path_buf(),
    )
    .await
    .expect("first fetch");
    let second = gated_costs_this_month_with_cache(
        &server.uri(),
        "test-admin-key",
        Duration::from_secs(30),
        dir.path().to_path_buf(),
    )
    .await
    .expect("cached fetch");

    assert_eq!(first, second);
    assert_eq!(first.total_micro_usd, 1_730_000);
    assert_eq!(first.by_line_item.len(), 2);
}

#[tokio::test]
async fn concurrent_same_identity_calls_send_at_most_one_get() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_string(body_typical()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let base_url = server.uri();
    let first = gated_costs_this_month_with_cache(
        &base_url,
        "test-admin-key",
        Duration::from_secs(30),
        root.clone(),
    );
    let second = gated_costs_this_month_with_cache(
        &base_url,
        "test-admin-key",
        Duration::from_secs(30),
        root,
    );
    let (a, b) = tokio::join!(first, second);
    assert!(a.is_ok() || b.is_ok());
}

#[tokio::test]
async fn stored_401_suppresses_the_second_request_and_keeps_auth_guidance() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"nope"}"#))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempdir().unwrap();

    let first = gated_costs_this_month_with_cache(
        &server.uri(),
        "bad-admin-key",
        Duration::from_secs(30),
        dir.path().to_path_buf(),
    )
    .await
    .expect_err("401");
    let second = gated_costs_this_month_with_cache(
        &server.uri(),
        "bad-admin-key",
        Duration::from_secs(30),
        dir.path().to_path_buf(),
    )
    .await
    .expect_err("stored 401");

    assert_eq!(first.failure_kind(), Some(StoredFailureKind::AuthInvalid));
    assert_eq!(second.failure_kind(), Some(StoredFailureKind::AuthInvalid));
    assert!(
        second
            .admin_key_hint()
            .is_some_and(|hint| hint.contains("HTTP 401"))
    );
}

#[tokio::test]
async fn stored_403_suppresses_the_second_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempdir().unwrap();

    for _ in 0..2 {
        let error = gated_costs_this_month_with_cache(
            &server.uri(),
            "wrong-scope-key",
            Duration::from_secs(30),
            dir.path().to_path_buf(),
        )
        .await
        .expect_err("403");
        assert_eq!(
            error.failure_kind(),
            Some(StoredFailureKind::InsufficientScope)
        );
        assert!(error.admin_key_hint().is_some());
    }
}

#[tokio::test]
async fn rate_limit_never_retries_inside_the_gate() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempdir().unwrap();

    for _ in 0..2 {
        let error = gated_costs_this_month_with_cache(
            &server.uri(),
            "test-admin-key",
            Duration::from_secs(30),
            dir.path().to_path_buf(),
        )
        .await
        .expect_err("429");
        assert_eq!(error.failure_kind(), Some(StoredFailureKind::RateLimited));
    }
}

#[tokio::test]
async fn transport_timeout_never_retries_inside_the_gate() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_string(body_typical()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempdir().unwrap();

    for _ in 0..2 {
        let error = gated_costs_this_month_with_cache(
            &server.uri(),
            "test-admin-key",
            Duration::from_millis(10),
            dir.path().to_path_buf(),
        )
        .await
        .expect_err("timeout");
        assert_eq!(error.failure_kind(), Some(StoredFailureKind::Network));
    }
}

#[tokio::test]
async fn redirects_are_not_followed_as_a_second_http_request() {
    let first_server = MockServer::start().await;
    let redirect_target = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "Location",
            format!("{}/v1/organization/costs", redirect_target.uri()),
        ))
        .expect(1)
        .mount(&first_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body_typical()))
        .expect(0)
        .mount(&redirect_target)
        .await;
    let dir = tempdir().unwrap();

    let error = gated_costs_this_month_with_cache(
        &first_server.uri(),
        "test-admin-key",
        Duration::from_secs(30),
        dir.path().to_path_buf(),
    )
    .await
    .expect_err("redirect must be classified without following it");
    assert_eq!(
        error.failure_kind(),
        Some(StoredFailureKind::UnexpectedStatus(302))
    );
}

#[tokio::test]
async fn server_and_shape_failures_each_send_one_get() {
    for (status, body, expected) in [
        (
            500,
            "internal server error",
            StoredFailureKind::UnexpectedStatus(500),
        ),
        (200, "not json", StoredFailureKind::ResponseShape),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/organization/costs"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempdir().unwrap();
        for _ in 0..2 {
            let error = gated_costs_this_month_with_cache(
                &server.uri(),
                "test-admin-key",
                Duration::from_secs(30),
                dir.path().to_path_buf(),
            )
            .await
            .expect_err("provider failure");
            assert_eq!(error.failure_kind(), Some(expected));
        }
    }
}

#[tokio::test]
async fn different_keys_have_independent_entries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body_typical()))
        .expect(2)
        .mount(&server)
        .await;
    let dir = tempdir().unwrap();

    for key in ["admin-key-a", "admin-key-b"] {
        gated_costs_this_month_with_cache(
            &server.uri(),
            key,
            Duration::from_secs(30),
            dir.path().to_path_buf(),
        )
        .await
        .expect("independent key fetch");
    }
}

#[tokio::test]
async fn same_key_with_different_api_bases_has_independent_entries() {
    let first_server = MockServer::start().await;
    let second_server = MockServer::start().await;
    for server in [&first_server, &second_server] {
        Mock::given(method("GET"))
            .and(path("/v1/organization/costs"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body_typical()))
            .expect(1)
            .mount(server)
            .await;
    }
    let dir = tempdir().unwrap();

    for base_url in [first_server.uri(), second_server.uri()] {
        gated_costs_this_month_with_cache(
            &base_url,
            "same-admin-key",
            Duration::from_secs(30),
            dir.path().to_path_buf(),
        )
        .await
        .expect("independent API base fetch");
    }
}

#[tokio::test]
async fn corrupt_or_unsupported_store_fails_closed_before_http() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body_typical()))
        .expect(0)
        .mount(&server)
        .await;

    for document in ["not json", r#"{"schema_version":999,"entries":{}}"#] {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("openai-cost.json"), document).unwrap();
        let error = gated_costs_this_month_with_cache(
            &server.uri(),
            "test-admin-key",
            Duration::from_secs(30),
            dir.path().to_path_buf(),
        )
        .await
        .expect_err("invalid gate store must fail closed");
        assert!(matches!(error, CostsGateError::Unavailable { .. }));
    }
}

#[tokio::test]
async fn gate_publication_failure_sends_zero_gets() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body_typical()))
        .expect(0)
        .mount(&server)
        .await;
    let dir = tempdir().unwrap();
    let file = dir.path().join("not-a-directory");
    std::fs::write(&file, b"x").unwrap();

    let error = gated_costs_this_month_with_cache(
        &server.uri(),
        "test-admin-key",
        Duration::from_secs(30),
        file,
    )
    .await
    .expect_err("gate must fail closed");
    assert!(matches!(error, CostsGateError::Unavailable { .. }));
}
