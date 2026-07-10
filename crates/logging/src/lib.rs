//! Shared `tracing` subscriber setup for the Balanze binaries.
//!
//! Both entry points (`balanze-cli` and the Tauri host) install the same
//! subscriber: a `BALANZE_LOG` env filter (defaulting to `info`) writing to
//! stderr (unless the caller opts out - the interactive `watch` TUI does, so
//! logs don't bleed over its alternate screen), plus - when a data directory is
//! resolvable - a daily-rotating file sink under `<data_dir>/logs/` (kept 3
//! days). See AGENTS.md §3.2/§3.4.
//!
//! This lives in its own crate rather than `settings` so the subscriber deps
//! (`tracing-subscriber`, `tracing-appender`) don't leak into the pure library
//! crates that depend on `settings` only for config.

use std::path::Path;

use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Install the process-wide `tracing` subscriber.
///
/// `filename_prefix` names the per-binary log file (e.g. `"balanze-cli"` /
/// `"balanze-gui"`). The two binaries MUST pass prefixes where neither is a
/// string-prefix of the other: `tracing-appender`'s retention pruning matches
/// candidate files by plain `starts_with(prefix)`, so an overlapping prefix
/// (the former "balanze") would let each binary's rotation sweep up and delete
/// the other's logs.
///
/// `to_stderr` attaches the stderr log layer. Pass `false` for a mode that owns
/// the terminal's alternate screen (the interactive `balanze-cli watch` TUI),
/// where stderr logs would paint over the UI; the rotating file sink then
/// captures the logs instead. All other invocations pass `true`. `to_stderr =
/// false` is honored only when a file sink is actually available - if no data
/// directory is resolvable, stderr is kept as a fallback so logs are never
/// silently dropped (a little TUI noise beats losing the trail).
///
/// Returns the file writer's [`WorkerGuard`], which the caller MUST hold for
/// the process lifetime; dropping it early stops the non-blocking file writer
/// from flushing. Returns `None` when no data directory is resolvable (the
/// subscriber is still installed and, in that case, always logs to stderr).
pub fn init_tracing(filename_prefix: &str, to_stderr: bool) -> Option<WorkerGuard> {
    // Distinguish "unset" (quietly default to info) from "set but invalid"
    // (a typo like `BALANZE_LOG=deubg` should not look identical to unset).
    let env_filter = match std::env::var("BALANZE_LOG") {
        Ok(directives) => tracing_subscriber::EnvFilter::try_new(&directives).unwrap_or_else(|e| {
            eprintln!("warning: invalid BALANZE_LOG={directives:?} ({e}); using \"info\"");
            tracing_subscriber::EnvFilter::new("info")
        }),
        Err(_) => tracing_subscriber::EnvFilter::new("info"),
    };
    let (file_layer, guard) = match build_file_writer(filename_prefix) {
        Some((writer, guard)) => {
            let layer = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer);
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };

    // Attach stderr when the caller asked for it, OR as a fallback when there is
    // no file sink - never leave the subscriber with zero output layers, which
    // would silently drop every log. `Option<Layer>` is itself a `Layer` (None
    // is a no-op), so this drops the sink cleanly without changing the shape.
    let stderr_layer = want_stderr(to_stderr, file_layer.is_some())
        .then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();

    guard
}

/// Whether to attach the stderr layer: when the caller asked for it, OR as a
/// fallback when there is no file sink (so the subscriber never ends up with
/// zero output layers, which would silently drop every log). Pure, so the
/// fallback invariant is testable without installing a process-global subscriber.
fn want_stderr(to_stderr: bool, has_file: bool) -> bool {
    to_stderr || !has_file
}

/// Resolve the data-dir log directory and open the rolling writer there, or
/// `None` when no data dir is resolvable. Thin wrapper over
/// [`build_rolling_writer`] so the latter stays testable with an explicit dir.
fn build_file_writer(filename_prefix: &str) -> Option<(NonBlocking, WorkerGuard)> {
    let dir = settings::log_dir()?;
    build_rolling_writer(&dir, filename_prefix)
}

/// Open a daily-rotating log file under `dir` and wrap it in a non-blocking
/// writer. `None` when the appender can't be opened (falls back to
/// stderr-only). Split out from [`init_tracing`] so this substantive file
/// plumbing (dir pre-create, rotation, 3-day retention) is unit-testable
/// without installing a process-global subscriber.
fn build_rolling_writer(dir: &Path, filename_prefix: &str) -> Option<(NonBlocking, WorkerGuard)> {
    // `Builder::build` prunes the directory for retention *before* creating it,
    // so on a brand-new install (no `logs/` yet) it prints an alarming-looking
    // "Error reading the log directory" to stderr. Pre-create it (best-effort -
    // a real failure surfaces through the `build` error below) so first run is
    // quiet too.
    let _ = std::fs::create_dir_all(dir);
    match tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(filename_prefix)
        .filename_suffix("log")
        .max_log_files(3)
        .build(dir)
    {
        Ok(appender) => Some(tracing_appender::non_blocking(appender)),
        Err(e) => {
            eprintln!("warning: could not open log file in {}: {e}", dir.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_rolling_writer, want_stderr};

    #[test]
    fn want_stderr_falls_back_when_no_file_sink() {
        // Caller asked for stderr -> always on, regardless of the file sink.
        assert!(want_stderr(true, true));
        assert!(want_stderr(true, false));
        // Caller opted out (watch TUI) -> off ONLY when a file sink exists.
        assert!(!want_stderr(false, true));
        // Opted out but no file sink -> fall back to stderr, never drop all logs.
        assert!(want_stderr(false, false));
    }

    #[test]
    fn build_rolling_writer_precreates_dir_and_opens() {
        let tmp = tempfile::tempdir().unwrap();
        let logs = tmp.path().join("logs");
        assert!(!logs.exists(), "precondition: logs dir does not exist yet");

        let out = build_rolling_writer(&logs, "balanze-test");

        assert!(
            out.is_some(),
            "should open a rolling writer in a fresh dir without error"
        );
        assert!(
            logs.is_dir(),
            "should pre-create the logs dir (quiet first run)"
        );
        // Hold then drop the guard so the worker thread flushes and joins.
        drop(out);
    }
}
