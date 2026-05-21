//! End-to-end integration test: write JSONL to a tempdir, spawn the watcher,
//! assert the coordinator snapshot reflects the data within 2 seconds.
//!
//! **Constraint:** this test mutates `HOME` / `USERPROFILE` environment
//! variables so that `find_claude_projects_dir()` discovers the tempdir tree.
//! Running multiple such tests in the same process in parallel would cause
//! a race. Since this is the only test file in the `watcher` crate, Cargo
//! runs all tests in this binary sequentially by default — no extra
//! `serial_test` dependency is needed.  If additional env-mutating tests are
//! added later, reach for `serial_test` or a `Mutex<()>` guard.

use std::path::PathBuf;
use std::time::Duration;

use tempfile::TempDir;
use tokio::time::sleep;

use state_coordinator::{spawn as spawn_coord, LogSink};
use watcher::Watcher;

/// Create a tempdir with `<tempdir>/.claude/projects/proj1/session.jsonl`
/// containing one well-formed `UsageEvent` line whose timestamp is within
/// the 5-hour rolling window (timestamp is ~1 minute ago relative to the
/// test run, formatted via `chrono`).
///
/// Returns `(TempDir, PathBuf)` — the TempDir keeps the tree alive; the
/// PathBuf is the JSONL file so the test can re-write it to force a notify
/// event.
///
/// **Side-effect:** sets `HOME` (POSIX) and `USERPROFILE` (Windows) env vars
/// so `find_claude_projects_dir()` discovers `<tempdir>/.claude/projects`.
fn setup_claude_jsonl_tree() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();

    // Override the home-dir env vars so the claude_parser walker resolves
    // to our tempdir. USERPROFILE is checked first on Windows; HOME on POSIX.
    // Both are set so the test is correct regardless of platform.
    // SAFETY: single-threaded test binary — see module-level constraint note.
    unsafe {
        std::env::set_var("USERPROFILE", dir.path());
        std::env::set_var("HOME", dir.path());
        // Clear XDG_CONFIG_HOME so it doesn't accidentally shadow our tempdir.
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    let projects_dir = dir.path().join(".claude").join("projects").join("proj1");
    std::fs::create_dir_all(&projects_dir).unwrap();

    let jsonl_path = projects_dir.join("session.jsonl");

    // Build a timestamp that is 1 minute before now, well within the 5-hour
    // rolling window.  We cannot use a hardcoded date because the window
    // math compares against `chrono::Utc::now()` at parse time.
    let ts = (chrono::Utc::now() - chrono::Duration::minutes(1))
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();

    // A minimal well-formed UsageEvent line. Schema derived from
    // `crates/claude_parser/src/parser.rs` and confirmed against the
    // committed fixture at
    // `crates/balanze_cli/tests/fixtures/claude/projects/test-project/session-001.jsonl`.
    let line = format!(
        r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"00000000-0000-7000-8000-000000000099","requestId":"req_watcher_test_001","uuid":"w-001","parentUuid":null,"cwd":"/tmp","entrypoint":"cli","gitBranch":"main","isSidechain":false,"userType":"external","version":"0.0.0-test","message":{{"id":"msg_watcher_test_001","role":"assistant","type":"message","model":"claude-sonnet-4-6","stop_reason":"end_turn","stop_sequence":null,"content":[{{"type":"text","text":"watcher integration test"}}],"usage":{{"input_tokens":100,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"service_tier":"standard"}}}}}}"#
    );
    std::fs::write(&jsonl_path, &line).unwrap();

    (dir, jsonl_path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jsonl_write_propagates_to_coordinator_snapshot() {
    let (_dir, jsonl_path) = setup_claude_jsonl_tree();

    let settings = settings::Settings::default();
    let (handle, _join) = spawn_coord(LogSink);
    let _tasks = Watcher::spawn(handle.clone(), &settings);

    // Give the watcher a moment to start and register the watch before
    // re-writing the file.  The initial scan (in `spawn`) will already pick
    // up the file; the re-write forces a notify event as a belt-and-suspenders
    // measure in case the watcher registered after the file was created.
    sleep(Duration::from_millis(100)).await;
    let content = std::fs::read(&jsonl_path).unwrap();
    std::fs::write(&jsonl_path, content).unwrap();

    // Poll until the coordinator snapshot has `claude_jsonl` populated, up to
    // a 2-second deadline.  The initial scan normally populates it within
    // milliseconds; 2 seconds is generous for slow CI environments.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let snap = handle.query().await.expect("coordinator alive");
        if let Some(jl) = snap.claude_jsonl {
            // Sanity-check: we wrote exactly 1 file, so files_scanned must be 1.
            assert_eq!(jl.files_scanned, 1, "expected 1 JSONL file scanned");
            // The event is within the 5h window, so we should see at least 1 event.
            assert!(
                jl.window.total_events_in_window >= 1,
                "expected at least 1 event in window, got {}",
                jl.window.total_events_in_window
            );
            return; // success
        }
        if std::time::Instant::now() > deadline {
            panic!("JSONL did not propagate to coordinator snapshot within 2 seconds");
        }
        sleep(Duration::from_millis(50)).await;
    }
}
