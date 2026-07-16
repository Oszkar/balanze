//! Safety poll task. Fires every 60 seconds, starting immediately. On each tick
//! it re-reads the statusline snapshot and the Codex quota and emits updates to
//! the coordinator. The statusline leg alone sits out the first tick (the
//! statusline notify task's startup read already covers it); Codex reads on the
//! first tick because this task is its only feeder.
//!
//! Purpose: catch filesystem events the statusline notify task might miss, and
//! poll Codex (which has no notify task of its own - its rollout dir isn't
//! watched). JSONL is NOT scanned here: its own notify task carries a 60s
//! incremental fallback, so there is one byte-cursor set and no periodic full
//! reparse (AGENTS.md §3.1). The OAuth and OpenAI cells are NOT re-fetched here:
//! they have dedicated 5-minute pollers, and re-hitting their endpoints on every
//! 60s safety tick would burn API quota.
//!
//! All sync I/O (`read_snapshot`, `read_codex_quota`) runs under
//! `tokio::task::spawn_blocking` so it doesn't block tokio worker threads.

use claude_statusline::{FileIoError, read_snapshot};
use codex_local::{ParseError, read_codex_quota};
use settings::statusline_snapshot_path;
use state_coordinator::{
    Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg, WatcherGeneration,
};
use tokio::task::JoinHandle;

use crate::errors::WatcherError;

/// Spawn the 60-second safety poll task and return its `JoinHandle`.
///
/// The first tick fires immediately, so Codex - whose only feeder this is - is
/// populated at launch rather than 60 seconds in. The statusline leg alone skips
/// that first tick, so the statusline notify task's own startup read isn't
/// duplicated within the first few milliseconds.
///
/// `codex_enabled` gates the per-tick Codex scan: when `false`, Codex is not
/// read or emitted (the Tauri host re-spawns the watcher on a settings change,
/// so the toggle applies live).
pub(crate) fn spawn(
    coord: StateCoordinatorHandle,
    codex_enabled: bool,
    generation: WatcherGeneration,
) -> JoinHandle<Result<(), WatcherError>> {
    tokio::spawn(async move {
        let statusline_path = statusline_snapshot_path();

        // `Delay` (not default `Burst`) so a long-running scan (deep
        // `~/.claude/projects/` tree) can't queue multiple missed 60s
        // ticks and fire blocking scans back-to-back on recovery.
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // The first tick is NOT skipped. Skipping it used to suppress the
        // statusline double-emit (the statusline notify task's own startup read
        // already covers that source) - but this is Codex's ONLY feeder, so
        // skipping the whole tick left the Codex cell blank for the first 60s of
        // every launch as collateral damage. Codex is local file I/O with no
        // provider endpoint behind it, so no politeness gate (AGENTS.md §3.1)
        // argues for the delay. Scope the suppression to the source that
        // actually needed it instead.
        let mut first_tick = true;

        loop {
            ticker.tick().await;
            tracing::debug!("watcher/safety: tick");

            // ── Statusline (skipped on the first tick; see `first_tick`) ──────
            if let Some(ref path) = statusline_path
                && !first_tick
            {
                let path_owned = path.clone();
                let read = tokio::task::spawn_blocking(move || read_snapshot(&path_owned)).await;

                match read {
                    Ok(Ok(payload)) => {
                        let _ = coord
                            .send(StateMsg::Update(SourceUpdate {
                                generation,
                                source: Source::ClaudeStatusline,
                                result: Ok(SourcePartial::ClaudeStatusline(payload)),
                            }))
                            .await;
                    }
                    Ok(Err(FileIoError::FileMissing { .. })) => {
                        // Not configured yet; no emit.
                        tracing::debug!(
                            "watcher/safety: statusline snapshot absent; skipping emit"
                        );
                    }
                    Ok(Err(e)) => {
                        let _ = coord
                            .send(StateMsg::Update(SourceUpdate {
                                generation,
                                source: Source::ClaudeStatusline,
                                result: Err(format!("{e}")),
                            }))
                            .await;
                    }
                    Err(join_err) => {
                        // Mirror the JSONL path: surface the panic to the
                        // coordinator as an Err Update so the UI can show
                        // the degraded state. Otherwise the snapshot keeps
                        // stale statusline data with no warning indicator.
                        tracing::error!(
                            "watcher/safety: statusline read task panicked: {join_err}"
                        );
                        let _ = coord
                            .send(StateMsg::Update(SourceUpdate {
                                generation,
                                source: Source::ClaudeStatusline,
                                result: Err(format!("statusline read task panicked: {join_err}")),
                            }))
                            .await;
                    }
                }
            }

            // ── Codex quota (gated on the toggle) ─────────────────────────────
            if codex_enabled {
                let codex = tokio::task::spawn_blocking(read_codex_quota).await;

                match codex {
                    Ok(res) => match codex_update(res) {
                        Some(result) => {
                            let _ = coord
                                .send(StateMsg::Update(SourceUpdate {
                                    generation,
                                    source: Source::CodexQuota,
                                    result,
                                }))
                                .await;
                        }
                        None => {
                            tracing::debug!(
                                "watcher/safety: codex not installed or no quota data; skipping emit"
                            );
                        }
                    },
                    Err(join_err) => {
                        // A panic is a genuine fault (unlike FileMissing) - surface it
                        // as degraded, consistent with the statusline panic path above.
                        tracing::error!("watcher/safety: codex read task panicked: {join_err}");
                        let _ = coord
                            .send(StateMsg::Update(SourceUpdate {
                                generation,
                                source: Source::CodexQuota,
                                result: Err(format!("codex read task panicked: {join_err}")),
                            }))
                            .await;
                    }
                }
            }

            first_tick = false;
        }
    })
}

/// Map a Codex quota read to an optional coordinator update.
///
/// `FileMissing` (the Codex CLI isn't installed - `~/.codex/sessions` is
/// absent) is a quiet not-configured state, NOT an error: it must not set
/// `codex_quota_error` or raise a degraded banner. Mirrors
/// `balanze_cli::live_fetch_codex_quota` and the `codex_local` error contract
/// (FileMissing => "not configured"; IoError / SchemaDrift => loud). Returns
/// `None` to skip the emit (keeping any prior value); `Ok(None)` (installed but
/// no quota data yet) is also a quiet skip.
fn codex_update(
    result: Result<Option<codex_local::CodexQuotaSnapshot>, ParseError>,
) -> Option<Result<SourcePartial, String>> {
    match result {
        Ok(Some(snap)) => Some(Ok(SourcePartial::CodexQuota(snap))),
        Ok(None) => None,
        Err(ParseError::FileMissing(_)) => None,
        Err(e) => Some(Err(format!("{e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::codex_update;
    use codex_local::ParseError;
    use std::path::PathBuf;

    #[test]
    fn codex_not_installed_is_quiet_not_an_error() {
        // FileMissing must NOT surface as a degraded error - it's the
        // "Codex not installed" not-configured state.
        let out = codex_update(Err(ParseError::FileMissing(PathBuf::from(
            "/home/u/.codex/sessions",
        ))));
        assert!(
            out.is_none(),
            "FileMissing should skip the emit, got {out:?}"
        );
    }

    #[test]
    fn codex_installed_no_data_is_quiet() {
        assert!(codex_update(Ok(None)).is_none());
    }

    #[test]
    fn codex_real_error_still_surfaces() {
        // A genuine filesystem error must still reach the degraded banner.
        let out = codex_update(Err(ParseError::IoError {
            path: PathBuf::from("/home/u/.codex/sessions"),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        }));
        match out {
            Some(Err(msg)) => assert!(!msg.is_empty()),
            other => panic!("expected Some(Err(..)), got {other:?}"),
        }
    }
}
