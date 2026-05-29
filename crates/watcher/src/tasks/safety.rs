//! Safety poll task. Fires every 60 seconds (first tick skipped — the JSONL
//! initial scan at startup already covered that window). On each tick it
//! re-runs the full JSONL scan + statusline read + Codex quota read and emits
//! updates to the coordinator.
//!
//! Purpose: catch filesystem events that the notify-based tasks might miss
//! (inotify exhaustion, atomic-rewrite detection lag, Codex session rollover).
//! The OAuth and OpenAI cells are NOT re-fetched here — those have dedicated
//! 5-minute pollers and re-hitting their endpoints on every 60s safety tick
//! would burn API quota (AGENTS.md §3.1).
//!
//! All sync I/O (`scan_events`, `read_snapshot`, `read_codex_quota`) runs
//! under `tokio::task::spawn_blocking` so it doesn't block tokio worker threads.

use std::path::PathBuf;
use std::sync::Arc;

use claude_parser::find_all_claude_projects_dirs;
use claude_statusline::{read_snapshot, FileIoError};
use codex_local::read_codex_quota;
use state_coordinator::{
    ClaudeJsonlInput, Source, SourcePartial, SourceUpdate, StateCoordinatorHandle, StateMsg,
};
use tokio::task::JoinHandle;

use super::jsonl::{scan_events, ScanResult};
use crate::errors::WatcherError;

// Re-export of the statusline snapshot-path helper — mirrored here so the
// safety task doesn't cross-import the private function from the statusline
// module. Both are identical.
// MIRRORS balanze_cli::statusline_snapshot_path and
//         watcher::tasks::statusline::statusline_snapshot_path — see
// TODO(v0.2-followup): extract live_fetch crate / shared paths helper.
fn statusline_snapshot_path() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("BALANZE_DATA_DIR_OVERRIDE") {
        return Some(PathBuf::from(env_path).join("statusline.snapshot.json"));
    }
    directories::ProjectDirs::from("me", "oszkar", "Balanze")
        .map(|d| d.data_dir().join("statusline.snapshot.json"))
}

/// Spawn the 60-second safety poll task and return its `JoinHandle`.
///
/// The first tick is intentionally skipped: `ticker.tick().await` is called
/// once before the loop to consume the immediate fire so the JSONL task's
/// initial scan at startup isn't duplicated by the safety poll within the
/// first few milliseconds.
pub(crate) fn spawn(coord: StateCoordinatorHandle) -> JoinHandle<Result<(), WatcherError>> {
    tokio::spawn(async move {
        // Resolve ALL JSONL project roots once — they don't change at runtime.
        // A dual-install machine can have both ~/.claude/projects and
        // ~/.config/claude/projects; empty ⇒ Claude Code not installed.
        let roots = find_all_claude_projects_dirs();
        if roots.is_empty() {
            tracing::warn!(
                "watcher/safety: no Claude projects dir found; \
                 JSONL/cost cells won't be safety-polled"
            );
        }

        let statusline_path = statusline_snapshot_path();

        // `Delay` (not default `Burst`) so a long-running scan (deep
        // `~/.claude/projects/` tree) can't queue multiple missed 60s
        // ticks and fire blocking scans back-to-back on recovery.
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Skip the first immediate tick — the JSONL task's startup scan
        // already covers this window. Without this skip the safety poll
        // would double-emit within milliseconds of app launch.
        ticker.tick().await;

        loop {
            ticker.tick().await;
            tracing::debug!("watcher/safety: tick");

            // ── JSONL (window + cost are derived in the coordinator) ──────────
            if !roots.is_empty() {
                let roots_owned = roots.clone();
                let scan = tokio::task::spawn_blocking(move || scan_events(&roots_owned)).await;

                match scan {
                    Ok(ScanResult::Ok {
                        events,
                        files_scanned,
                    }) => {
                        let _ = coord
                            .send(StateMsg::Update(SourceUpdate {
                                source: Source::ClaudeJsonl,
                                result: Ok(SourcePartial::ClaudeJsonl(ClaudeJsonlInput {
                                    events: Arc::new(events),
                                    files_scanned,
                                })),
                            }))
                            .await;
                    }
                    Ok(ScanResult::Fatal { error }) => {
                        tracing::warn!("watcher/safety: JSONL scan fatal: {error}");
                        let _ = coord
                            .send(StateMsg::Update(SourceUpdate {
                                source: Source::ClaudeJsonl,
                                result: Err(error),
                            }))
                            .await;
                    }
                    Err(join_err) => {
                        tracing::error!("watcher/safety: JSONL scan task panicked: {join_err}");
                        let _ = coord
                            .send(StateMsg::Update(SourceUpdate {
                                source: Source::ClaudeJsonl,
                                result: Err(format!("scan task panicked: {join_err}")),
                            }))
                            .await;
                    }
                }
            }

            // ── Statusline ────────────────────────────────────────────────────
            if let Some(ref path) = statusline_path {
                let path_owned = path.clone();
                let read = tokio::task::spawn_blocking(move || read_snapshot(&path_owned)).await;

                match read {
                    Ok(Ok(payload)) => {
                        let _ = coord
                            .send(StateMsg::Update(SourceUpdate {
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
                                source: Source::ClaudeStatusline,
                                result: Err(format!("statusline read task panicked: {join_err}")),
                            }))
                            .await;
                    }
                }
            }

            // ── Codex quota ───────────────────────────────────────────────────
            let codex = tokio::task::spawn_blocking(read_codex_quota).await;

            match codex {
                Ok(Ok(Some(snap))) => {
                    let _ = coord
                        .send(StateMsg::Update(SourceUpdate {
                            source: Source::CodexQuota,
                            result: Ok(SourcePartial::CodexQuota(snap)),
                        }))
                        .await;
                }
                Ok(Ok(None)) => {
                    // Codex installed but no quota data yet — keep prior value.
                    tracing::debug!("watcher/safety: codex quota absent; skipping emit");
                }
                Ok(Err(e)) => {
                    let _ = coord
                        .send(StateMsg::Update(SourceUpdate {
                            source: Source::CodexQuota,
                            result: Err(format!("{e}")),
                        }))
                        .await;
                }
                Err(join_err) => {
                    tracing::error!("watcher/safety: codex read task panicked: {join_err}");
                }
            }
        }
    })
}
