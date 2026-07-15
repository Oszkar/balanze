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

/// A settings.json whose statusline template asks for `{openai_cost}`. Without
/// this the segment is off by default and the fetch is demand-gated away.
fn settings_json_requesting_openai() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "statusline": {
            "lines": ["{context_bar} {cost} {usage} {codex} {openai_cost}"]
        }
    })
}

/// Run `balanze-cli statusline` once against the given dirs. `config_dir` is
/// `None` to exercise the shipped default template.
fn run_statusline(
    data: &std::path::Path,
    cache: &std::path::Path,
    codex: &std::path::Path,
    base: &str,
    config: Option<&std::path::Path>,
) -> std::process::Output {
    let mut cmd = Command::cargo_bin("balanze-cli").unwrap();
    cmd.arg("statusline")
        .env("BALANZE_DATA_DIR_OVERRIDE", data)
        .env("BALANZE_CACHE_DIR_OVERRIDE", cache)
        .env("BALANZE_OPENAI_API_BASE", base)
        .env("BALANZE_OPENAI_KEY", "sk-test")
        .env("CODEX_CONFIG_DIR", codex)
        .env("NO_COLOR", "1")
        .write_stdin(r#"{"version":"2.1.144","model":{"display_name":"Sonnet"}}"#);
    match config {
        Some(dir) => cmd.env("BALANZE_CONFIG_DIR_OVERRIDE", dir),
        // An empty temp dir has no settings.json -> the shipped defaults load.
        None => cmd.env("BALANZE_CONFIG_DIR_OVERRIDE", data),
    };
    cmd.output().unwrap()
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
    let config_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        config_dir.path().join("settings.json"),
        serde_json::to_vec(&settings_json_requesting_openai()).unwrap(),
    )
    .unwrap();
    let base = server.uri();

    // Two renders within the TTL: the cache must yield exactly one upstream GET.
    for _ in 0..2 {
        let (data, cache, codex, config, base) = (
            data_dir.path().to_path_buf(),
            cache_dir.path().to_path_buf(),
            codex_dir.path().to_path_buf(),
            config_dir.path().to_path_buf(),
            base.clone(),
        );
        let out = tokio::task::spawn_blocking(move || {
            run_statusline(&data, &cache, &codex, &base, Some(&config))
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
            stdout.contains("🌀 $4.20"),
            "self-composed OpenAI segment missing;\nstdout: {stdout}\nstderr: {stderr}"
        );
    }
    // `server` drops here; `.expect(1)` is verified on drop ->
    // two renders, one fetch proves the 300s gate.
}

/// The demand gate: with the shipped default template the OpenAI segment is not
/// rendered, so the billing API must not be called at all - not once, not
/// cached. This is the regression test for the gate.
#[tokio::test]
async fn default_template_never_fetches_openai() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(costs_body()))
        .expect(0) // no line asks for the segment -> no upstream call
        .mount(&server)
        .await;

    let data_dir = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let codex_dir = tempfile::tempdir().unwrap();
    let base = server.uri();

    let (data, cache, codex, b) = (
        data_dir.path().to_path_buf(),
        cache_dir.path().to_path_buf(),
        codex_dir.path().to_path_buf(),
        base.clone(),
    );
    let out = tokio::task::spawn_blocking(move || run_statusline(&data, &cache, &codex, &b, None))
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
        !stdout.contains("🌀 $"),
        "the OpenAI cost segment must not render under the default template;\nstdout: {stdout}"
    );
    // `server` drops here; `.expect(0)` is verified on drop.
}
