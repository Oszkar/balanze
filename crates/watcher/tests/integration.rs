//! End-to-end integration test: write JSONL to a tempdir, spawn the watcher,
//! assert the coordinator snapshot reflects the data, then APPEND a second
//! event and assert the incremental byte-cursor path picks it up - all within a
//! few seconds.
//!
//! **Constraint:** this test mutates `HOME` / `USERPROFILE` environment
//! variables so that `find_claude_projects_dir()` discovers the tempdir
//! tree. `libtest` runs tests within a single binary in parallel by
//! default; today this file contains exactly one test, so no env-var
//! race is possible. If a second env-mutating test is added later, both
//! must serialize on a `Mutex<()>` guard (or pull in `serial_test`) -
//! adding one without the other is a flake waiting to happen.
//!
//! The test uses `#[tokio::test(flavor = "current_thread")]` so that
//! `set_var`/`remove_var` runs on a process where no tokio worker
//! threads have been spawned yet (the current_thread runtime drives
//! the future on the calling thread without spawning workers). The
//! notify-callback thread is created later, inside `Watcher::spawn`,
//! after the env vars are already in their final state.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use tempfile::TempDir;
use tokio::time::sleep;

use state_coordinator::{LogSink, StateCoordinatorHandle, spawn as spawn_coord};
use watcher::Watcher;

/// A minimal well-formed, newline-terminated assistant `UsageEvent` line whose
/// timestamp is ~1 minute ago (well within the 5-hour rolling window). Claude
/// Code writes `\n`-terminated lines, and the incremental parser only parses a
/// line once its terminating `\n` lands - so the trailing newline is load-
/// bearing for this test, not decoration.
fn assistant_line(msg_id: &str, req_id: &str) -> String {
    let ts = (chrono::Utc::now() - chrono::Duration::minutes(1))
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"00000000-0000-7000-8000-000000000099","requestId":"{req_id}","uuid":"w-{msg_id}","message":{{"id":"{msg_id}","role":"assistant","model":"claude-sonnet-4-6","usage":{{"input_tokens":100,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}{newline}"#,
        newline = "\n"
    )
}

/// Create a tempdir with `<tempdir>/.claude/projects/proj1/session.jsonl`
/// containing one well-formed event line. Returns `(TempDir, PathBuf)` - the
/// TempDir keeps the tree alive; the PathBuf is the JSONL file so the test can
/// append to it.
///
/// **Side-effect:** sets `HOME` (POSIX) and `USERPROFILE` (Windows) env vars
/// so `find_claude_projects_dir()` discovers `<tempdir>/.claude/projects`.
fn setup_claude_jsonl_tree() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();

    // Override the home-dir env vars so the claude_parser walker resolves
    // to our tempdir. USERPROFILE is checked first on Windows; HOME on POSIX.
    //
    // SAFETY (per Rust 1.84+ env-mutation rules): this test runs under
    // `#[tokio::test(flavor = "current_thread")]` (see module doc), which
    // does NOT spawn worker threads. The notify-callback thread, the
    // only other thread that could read env, is created later inside
    // `Watcher::spawn`. So at the moment of these `set_var` / `remove_var`
    // calls, the test process is single-threaded and no concurrent
    // env read can race with the write.
    unsafe {
        std::env::set_var("USERPROFILE", dir.path());
        std::env::set_var("HOME", dir.path());
        // Clear XDG_CONFIG_HOME so it doesn't accidentally shadow our tempdir.
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    let projects_dir = dir.path().join(".claude").join("projects").join("proj1");
    std::fs::create_dir_all(&projects_dir).unwrap();

    let jsonl_path = projects_dir.join("session.jsonl");
    std::fs::write(
        &jsonl_path,
        assistant_line("msg_watcher_test_001", "req_watcher_test_001"),
    )
    .unwrap();

    (dir, jsonl_path)
}

/// Poll the coordinator snapshot until `claude_jsonl` reports at least
/// `min_events` events in the window, asserting `files_scanned == 1`. Panics on
/// a 3-second deadline.
async fn wait_for_events(handle: &StateCoordinatorHandle, min_events: usize) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let snap = handle.query().await.expect("coordinator alive");
        if let Some(jl) = snap.claude_jsonl {
            if jl.window.total_events_in_window >= min_events {
                assert_eq!(jl.files_scanned, 1, "expected exactly 1 JSONL file scanned");
                return;
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("expected >= {min_events} events in window within 3 seconds");
        }
        sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn jsonl_initial_scan_then_incremental_append_propagate() {
    let (_dir, jsonl_path) = setup_claude_jsonl_tree();

    let settings = settings::Settings::default();
    let (handle, _join) = spawn_coord(LogSink);
    handle
        .transition_settings(settings.clone(), 1)
        .await
        .unwrap();
    // Keep alive for the test duration: dropping the `Vec<JoinHandle>` would
    // not cancel the spawned tasks (tokio task lifetime is independent of
    // JoinHandle), but holding it makes the intent explicit.
    let _tasks: Vec<_> = Watcher::spawn(handle.clone(), &settings, 1);

    // Phase 1 - the launch scan reads the existing file in full (the one full
    // read the byte cursor allows), so the first event propagates.
    wait_for_events(&handle, 1).await;

    // Phase 2 - APPEND a second event. A true append (the file grows) is the
    // incremental path: the byte cursor resumes after the first line and reads
    // ONLY the newly-appended line, never re-reading the whole file. The
    // coordinator's event set must grow to 2.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&jsonl_path)
        .unwrap();
    f.write_all(assistant_line("msg_watcher_test_002", "req_watcher_test_002").as_bytes())
        .unwrap();
    f.flush().unwrap();
    drop(f);

    wait_for_events(&handle, 2).await;
}
