//! Self-compose end-to-end: no snapshot present, OpenAI composed directly and
//! gated to one fetch per 300s. Drives the real `balanze-cli statusline` binary
//! against a wiremock Admin Costs API.

use assert_cmd::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Minimal valid `/v1/organization/costs` body. Shape matches what
/// `openai_client` parses (verified against `crates/openai_client/tests/`);
/// value 4.20 USD -> total_micro_usd 4_200_000 -> rendered "🌀 $4.20".
fn costs_body() -> serde_json::Value {
    serde_json::json!({
        "object": "page",
        "data": [{
            "object": "bucket",
            "start_time": 0,
            "end_time": 1,
            "results": [{
                "object": "organization.costs.result",
                "amount": { "value": 4.20, "currency": "usd" },
                "line_item": "gpt-5"
            }]
        }],
        "has_more": false
    })
}

#[tokio::test]
async fn self_compose_renders_openai_and_gates_to_one_fetch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(costs_body()))
        .expect(1) // the 300s cache must collapse two renders into one fetch
        .mount(&server)
        .await;

    let data_dir = tempfile::tempdir().unwrap(); // empty -> no snapshot.json -> self-compose
    let cache_dir = tempfile::tempdir().unwrap();
    let codex_dir = tempfile::tempdir().unwrap(); // empty -> Codex absent, focus on OpenAI
    let base = server.uri();

    // Two renders within the TTL: the cache must yield exactly one upstream GET.
    for _ in 0..2 {
        let (data, cache, codex, base) = (
            data_dir.path().to_path_buf(),
            cache_dir.path().to_path_buf(),
            codex_dir.path().to_path_buf(),
            base.clone(),
        );
        let out = tokio::task::spawn_blocking(move || {
            Command::cargo_bin("balanze-cli")
                .unwrap()
                .arg("statusline")
                .env("BALANZE_DATA_DIR_OVERRIDE", &data)
                .env("BALANZE_CACHE_DIR_OVERRIDE", &cache)
                .env("BALANZE_OPENAI_API_BASE", &base)
                .env("BALANZE_OPENAI_KEY", "sk-test")
                .env("CODEX_CONFIG_DIR", &codex)
                .env("NO_COLOR", "1")
                .write_stdin(r#"{"version":"2.1.144","model":{"display_name":"Sonnet"}}"#)
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "balanze-cli statusline exited {:?};\nstderr: {stderr}\nstdout: {stdout}",
            out.status,
        );
        assert!(
            stdout.contains("🌀 $"),
            "self-composed OpenAI segment missing;\nstdout: {stdout}\nstderr: {stderr}"
        );
    }
    // `server` drops here; `.expect(1)` is verified on drop ->
    // two renders, one fetch proves the 300s gate.
}
