//! Bounded ratatui TUI for the `watch` command.
//!
//! Entered only when stdout `IsTerminal` and `--json` is absent (see
//! `watch_cmd::run_watch_mode`). The non-TTY / `--json` paths keep the existing
//! `StdoutSink` / `JsonlSink` behavior unchanged.
//!
//! Architecture (spec section 7):
//! - `ChannelSink` is the coordinator `Sink`. It republishes the latest
//!   `Snapshot` into a `tokio::sync::watch` channel (watch keeps only the
//!   newest value, which is exactly right for a single-screen UI).
//! - `run_tui` selects over `watch.changed()` and a `crossterm` async
//!   `EventStream`, drawing on every change/resize and handling keys.
//! - `TerminalGuard` (RAII) owns raw mode + the alternate screen and restores
//!   on `Drop`; a chained panic hook restores first. ALL exit paths drop the
//!   guard, so the user's shell is never left garbled.

use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use state_coordinator::{Sink, Snapshot, Source, StateCoordinatorHandle, StateMsg};
use tokio::sync::watch;

use crate::format::micro_usd_to_display_dollars;
use crate::present::{Bucket, bucket_for_fraction};

// ---------------------------------------------------------------------------
// ChannelSink: republish snapshots into a watch channel.
// ---------------------------------------------------------------------------

/// A [`Sink`] that republishes the latest `Snapshot` into a `watch` channel.
///
/// The `watch` channel coalesces: only the most recent snapshot is retained,
/// which matches a glanceable single-screen UI (no per-event backlog). The
/// receiver end is handed to [`run_tui`].
pub struct ChannelSink {
    tx: watch::Sender<Option<Snapshot>>,
}

impl ChannelSink {
    /// Construct the sink and its paired receiver, seeded with `None`
    /// (cold-start: nothing observed yet).
    pub fn new() -> (Self, watch::Receiver<Option<Snapshot>>) {
        let (tx, rx) = watch::channel(None);
        (Self { tx }, rx)
    }
}

impl Sink for ChannelSink {
    fn on_snapshot(&mut self, snapshot: &Snapshot) {
        // `Snapshot` has no PartialEq (intentional - snapshot.rs), so we cannot
        // dedup here; `send_replace` overwrites unconditionally and the render
        // loop coalesces via `watch.changed()`. Cloning is cheap relative to the
        // 60s safety-poll / 5-min OAuth cadence.
        self.tx.send_replace(Some(snapshot.clone()));
    }

    fn on_degraded(&mut self, _source: Source, _error: &str) {
        // No-op: this carries no `Snapshot`, and the coordinator records the
        // error in its snapshot WITHOUT a following `on_snapshot` (a pure-failure
        // update fires only `on_degraded`). The degraded banner is instead pulled
        // in by `run_tui`'s periodic `StateMsg::Refresh` tick, which re-notifies
        // the current (error-bearing) snapshot into the channel within one tick.
    }
}

// ---------------------------------------------------------------------------
// TerminalGuard: RAII raw mode + alternate screen, with a restoring panic hook.
// ---------------------------------------------------------------------------

/// Tracks whether the terminal is currently in raw-mode + alt-screen so the
/// restore routine is idempotent: the panic hook and `Drop` can both fire
/// without double-leaving the alternate screen.
static TERMINAL_ENTERED: AtomicBool = AtomicBool::new(false);

/// Leave the alternate screen and disable raw mode if we entered them.
/// Idempotent and infallible-by-design: a restore on an already-restored
/// terminal is a no-op, and any underlying I/O error is swallowed because the
/// alternatives (panic-in-Drop, panic-in-panic-hook) are strictly worse.
fn restore_terminal() {
    if !TERMINAL_ENTERED.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout();
    // Leave alt screen first, then disable raw mode (reverse of enter).
    let _ = execute!(out, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

/// Install a panic hook that restores the terminal BEFORE the previously
/// installed hook prints the panic message, so the message lands on the normal
/// screen instead of a garbled alt screen. Chained (not replaced) so the
/// default backtrace/abort behavior is preserved.
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        prev(info);
    }));
}

/// RAII terminal state: raw mode + alternate screen on `enter`, restored on
/// `Drop`. ALL exit paths in `run_tui` (and its callers) must hold this guard
/// so the user's shell is never left in raw/alt-screen mode.
pub struct TerminalGuard {
    /// Owned stdout handle the ratatui backend draws through.
    out: Stdout,
}

impl TerminalGuard {
    /// Enter raw mode + alternate screen and install the restoring panic hook.
    /// Returns the guard; dropping it restores the terminal.
    pub fn enter() -> anyhow::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        // Mark entered as soon as raw mode is live, BEFORE the alt-screen write:
        // a panic during the window (e.g. on another tokio worker thread - the
        // coordinator + watcher are already spawned) must still route through
        // restore_terminal() and disable raw mode. The restore's
        // LeaveAlternateScreen is a harmless no-op if the alt screen was never
        // entered.
        TERMINAL_ENTERED.store(true, Ordering::SeqCst);
        let mut out = io::stdout();
        // If entering the alt screen fails, undo raw mode and clear the flag
        // before bailing so a later Drop/hook doesn't try to leave a screen we
        // never entered.
        if let Err(e) = execute!(out, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            TERMINAL_ENTERED.store(false, Ordering::SeqCst);
            return Err(e.into());
        }
        Ok(Self { out })
    }

    /// Borrow the owned stdout for constructing the ratatui backend.
    pub fn stdout(&mut self) -> &mut Stdout {
        &mut self.out
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

// ---------------------------------------------------------------------------
// Render: Snapshot -> one ratatui frame.
// ---------------------------------------------------------------------------

/// Map the shared presentation `Bucket` (PR2) to a ratatui gauge color. Reuses
/// the tray's threshold semantics so the TUI and the colored text renderer
/// cannot diverge.
fn bucket_color(b: Bucket) -> Color {
    match b {
        Bucket::Ok => Color::Green,
        Bucket::Warn => Color::Yellow,
        Bucket::Critical => Color::Red,
        Bucket::Neutral => Color::DarkGray,
    }
}

/// Render one labeled gauge row: a bar colored by the utilization fraction
/// (0.0..=1.0+) with `<label> NN.N%` centered on it. `percent` is
/// Anthropic-style 0..100. The label and percent share the gauge's own centered
/// label (NOT a Block title) because each gauge occupies a single 1-row area - a
/// titled Block would eat that row and leave no room for the bar.
fn quota_gauge(label: &str, percent: f32) -> Gauge<'static> {
    let frac = (percent / 100.0).clamp(0.0, 1.0) as f64;
    let bucket = bucket_for_fraction(percent as f64 / 100.0);
    Gauge::default()
        .gauge_style(Style::default().fg(bucket_color(bucket)))
        .ratio(frac)
        .label(format!("{label} {percent:.1}%"))
}

/// Short cadence label (`5h` / `7d`) from a raw cadence key. Mirrors render.rs
/// `short_cadence` family-prefix logic so the two views agree.
fn short_cadence_label(key: &str) -> &'static str {
    if key.starts_with("five_hour") {
        "5h"
    } else if key.starts_with("seven_day") {
        "7d"
    } else {
        "?"
    }
}

/// Collect the human names of every source currently carrying an error. Drives
/// the degraded banner. Empty vec => no banner.
fn degraded_sources(s: &Snapshot) -> Vec<&'static str> {
    let mut out = Vec::new();
    if s.claude_oauth_error.is_some() {
        out.push("ClaudeOAuth");
    }
    if s.claude_jsonl_error.is_some() {
        out.push("ClaudeJsonl");
    }
    if s.anthropic_api_cost_error.is_some() {
        out.push("AnthropicApiCost");
    }
    if s.codex_quota_error.is_some() {
        out.push("CodexQuota");
    }
    if s.openai_error.is_some() {
        out.push("OpenAiCosts");
    }
    if s.claude_statusline_error.is_some() {
        out.push("ClaudeStatusline");
    }
    out
}

/// Render the entire single-screen TUI for one `Snapshot`. Layout (top to
/// bottom): title+clock, Anthropic block, OpenAI block, pace line, leverage
/// line, degraded banner (only when a source errored), keybind footer.
pub fn draw_ui(frame: &mut Frame, s: &Snapshot) {
    let degraded = degraded_sources(s);
    let banner_h = if degraded.is_empty() { 0 } else { 1 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),        // title + clock
            Constraint::Length(5),        // Anthropic block (border + 2 gauges + $)
            Constraint::Length(4),        // OpenAI block (border + gauge + $)
            Constraint::Length(1),        // pace line
            Constraint::Length(1),        // leverage line
            Constraint::Length(banner_h), // degraded banner (0 when clean)
            Constraint::Min(0),           // filler
            Constraint::Length(1),        // keybind footer
        ])
        .split(frame.area());

    draw_title(frame, chunks[0], s);
    draw_anthropic(frame, chunks[1], s);
    draw_openai(frame, chunks[2], s);
    draw_pace(frame, chunks[3], s);
    draw_leverage(frame, chunks[4], s);
    if !degraded.is_empty() {
        draw_degraded_banner(frame, chunks[5], &degraded);
    }
    draw_footer(frame, chunks[7]);
}

fn draw_title(frame: &mut Frame, area: Rect, s: &Snapshot) {
    let line = Line::from(vec![
        Span::styled(
            "Balanze watch",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::raw(format!("updated {}", s.fetched_at.format("%H:%M:%S UTC"))),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_anthropic(frame: &mut Frame, area: Rect, s: &Snapshot) {
    let block = Block::default().borders(Borders::ALL).title("Anthropic");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // 5h gauge
            Constraint::Length(1), // 7d gauge
            Constraint::Length(1), // extra usage $
        ])
        .split(inner);

    match &s.claude_oauth {
        Some(oauth) if !oauth.cadences.is_empty() => {
            // First two cadences (pre-sorted 5h, 7d by anthropic_oauth).
            for (i, c) in oauth.cadences.iter().take(2).enumerate() {
                let label = short_cadence_label(&c.key);
                frame.render_widget(quota_gauge(label, c.utilization_percent), rows[i]);
            }
            // Extra-usage overage (real billed), only when enabled.
            let eu_line = match oauth.extra_usage.as_ref().filter(|eu| eu.is_enabled) {
                Some(eu) => format!(
                    "extra usage {}/{} (real billed)",
                    micro_usd_to_display_dollars(eu.used_credits_micro_usd),
                    micro_usd_to_display_dollars(eu.monthly_limit_micro_usd),
                ),
                None => "extra usage: not enabled".to_string(),
            };
            frame.render_widget(Paragraph::new(eu_line), rows[2]);
        }
        Some(_) => {
            frame.render_widget(Paragraph::new("ready (no cadence bars)"), rows[0]);
        }
        None => {
            let msg = if s.claude_oauth_error.is_some() {
                "oauth fetch failed"
            } else {
                "not configured"
            };
            frame.render_widget(Paragraph::new(msg), rows[0]);
        }
    }
}

fn draw_openai(frame: &mut Frame, area: Rect, s: &Snapshot) {
    let block = Block::default().borders(Borders::ALL).title("OpenAI");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // codex gauge
            Constraint::Length(1), // admin $
        ])
        .split(inner);

    match &s.codex_quota {
        Some(q) => {
            frame.render_widget(quota_gauge("Codex", q.primary.used_percent as f32), rows[0]);
        }
        None => {
            let msg = if s.codex_quota_error.is_some() {
                "codex read error"
            } else {
                "Codex: not configured"
            };
            frame.render_widget(Paragraph::new(msg), rows[0]);
        }
    }

    let admin_line = match &s.openai {
        Some(costs) => format!(
            "admin costs {}",
            micro_usd_to_display_dollars(costs.total_micro_usd)
        ),
        None if s.openai_error.is_some() => "admin costs: fetch failed".to_string(),
        None => "admin costs: not configured".to_string(),
    };
    frame.render_widget(Paragraph::new(admin_line), rows[1]);
}

fn draw_pace(frame: &mut Frame, area: Rect, s: &Snapshot) {
    if s.pace.is_empty() {
        frame.render_widget(Paragraph::new("Pace: -"), area);
        return;
    }
    let parts: Vec<String> = s
        .pace
        .iter()
        .map(|p| {
            let ratio = match p.ratio {
                Some(r) => format!("{r:.1}x"),
                None => "-".to_string(),
            };
            format!(
                "{} {:.0}% used / {:.0}% elapsed ({ratio})",
                short_cadence_label(&p.key),
                p.used_fraction * 100.0,
                p.elapsed_fraction * 100.0,
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(format!("Pace: {}", parts.join(";  "))), area);
}

fn draw_leverage(frame: &mut Frame, area: Rect, s: &Snapshot) {
    let line = match &s.anthropic_api_cost {
        Some(cost) if cost.total_event_count > 0 => format!(
            "Leverage: ~{} of Claude Code usage at list prices (NOT billed)",
            micro_usd_to_display_dollars(cost.total_micro_usd)
        ),
        _ if s.anthropic_api_cost_error.is_some() => "Leverage: cost synthesis failed".to_string(),
        _ if s.claude_jsonl_error.is_some() => "Leverage: jsonl load failed".to_string(),
        _ => "Leverage: -".to_string(),
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_degraded_banner(frame: &mut Frame, area: Rect, sources: &[&str]) {
    let line = format!("DEGRADED: {} (showing stale data)", sources.join(", "));
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(Color::Black).bg(Color::Yellow)),
        area,
    );
}

fn draw_footer(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("q/Esc quit   r refresh").style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

// ---------------------------------------------------------------------------
// Event loop.
// ---------------------------------------------------------------------------

/// What a key event means to the TUI loop. Pure mapping so it is testable
/// without a real terminal / EventStream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Quit,
    Refresh,
    Ignore,
}

fn classify_key(key: KeyEvent) -> Action {
    // Press-only: crossterm on Windows (a primary target) emits a Press AND a
    // Release for every keystroke, plus Repeat while held. Acting on all of them
    // would fire `r` (Refresh) twice per press. The Quit keys are unaffected (the
    // loop breaks on Press, never reading the Release), but gating here keeps the
    // mapping honest and unit-testable.
    if key.kind != KeyEventKind::Press {
        return Action::Ignore;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => Action::Quit,
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => Action::Quit,
        (KeyCode::Char('r'), _) => Action::Refresh,
        _ => Action::Ignore,
    }
}

/// Why [`run_tui`] returned. Lets the supervisor distinguish a user-initiated
/// quit from the coordinator dropping the snapshot channel - the latter would
/// otherwise look like a clean quit and hide a coordinator failure when the
/// `run_tui` arm wins the supervisor's `select!` race against the join handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiExit {
    /// The user quit (q / Esc / Ctrl-C) or the input stream ended.
    UserQuit,
    /// The snapshot channel closed: the state coordinator dropped its sink
    /// (exited or panicked). The supervisor surfaces this as a fatal.
    CoordinatorGone,
}

/// How often the TUI asks the coordinator to re-notify (`StateMsg::Refresh`).
/// A provider FAILURE records its error in the coordinator's snapshot and fires
/// only `on_degraded` (no `on_snapshot`), so the watch channel would not update
/// on a pure failure and the degraded banner would stay dead. The periodic
/// Refresh pulls the coordinator's current (error-bearing) snapshot into the
/// channel, surfacing the degraded state within one tick. Refresh is a
/// re-notify, not a re-fetch, so it generates no provider traffic.
const REPAINT_INTERVAL_SECS: u64 = 2;

/// Drive the TUI until the user quits or the coordinator drops the channel.
/// Selects over snapshot updates (`watch.changed()`), terminal input (crossterm
/// `EventStream`), and a Refresh tick. The `TerminalGuard` is owned here and
/// dropped on every return path, restoring the terminal. `r` (and the tick)
/// send `StateMsg::Refresh` (a re-paint, not a re-fetch - see messages.rs).
pub async fn run_tui(
    mut rx: watch::Receiver<Option<Snapshot>>,
    handle: StateCoordinatorHandle,
) -> anyhow::Result<TuiExit> {
    let mut guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(guard.stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut events = EventStream::new();
    let mut repaint = tokio::time::interval(std::time::Duration::from_secs(REPAINT_INTERVAL_SECS));

    // Initial paint from whatever the channel currently holds.
    {
        let snap = rx.borrow().clone();
        let snap = snap.unwrap_or_else(|| Snapshot::empty(Utc::now()));
        terminal.draw(|f| draw_ui(f, &snap))?;
    }

    let outcome = loop {
        tokio::select! {
            // A newer snapshot arrived.
            changed = rx.changed() => {
                if changed.is_err() {
                    // Sender dropped: the coordinator exited/panicked. Report it
                    // distinctly so the supervisor surfaces a fatal even if this
                    // arm wins the race against the coordinator join handle.
                    break TuiExit::CoordinatorGone;
                }
                let snap = rx.borrow_and_update().clone();
                let snap = snap.unwrap_or_else(|| Snapshot::empty(Utc::now()));
                terminal.draw(|f| draw_ui(f, &snap))?;
            }
            // Periodic re-notify: pulls the coordinator's current snapshot (incl.
            // any error slots set via on_degraded-only failure updates) into the
            // channel; the resulting `rx.changed()` drives the redraw. Best-effort
            // - a closed coordinator is caught by the changed() arm.
            _ = repaint.tick() => {
                let _ = handle.send(StateMsg::Refresh).await;
            }
            // A terminal event arrived.
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => match classify_key(key) {
                        Action::Quit => break TuiExit::UserQuit,
                        Action::Refresh => {
                            let _ = handle.send(StateMsg::Refresh).await;
                        }
                        Action::Ignore => {}
                    },
                    Some(Ok(Event::Resize(_, _))) => {
                        let snap = rx.borrow().clone();
                        let snap = snap.unwrap_or_else(|| Snapshot::empty(Utc::now()));
                        terminal.draw(|f| draw_ui(f, &snap))?;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        // Input stream error: restore + bubble up. `guard` drops
                        // on the early return, restoring the terminal.
                        return Err(e.into());
                    }
                    None => break TuiExit::UserQuit, // EventStream ended.
                }
            }
        }
    };

    // Explicit drop documents the restore boundary; Drop would fire anyway.
    drop(terminal);
    drop(guard);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    use anthropic_oauth::{CadenceBar, ClaudeOAuthSnapshot, ExtraUsage};
    use codex_local::{CodexQuotaSnapshot, RateLimitWindow};
    use openai_client::OpenAiCosts;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    // -- ChannelSink -------------------------------------------------------

    #[test]
    fn channel_sink_publishes_latest_snapshot() {
        let (mut sink, rx) = ChannelSink::new();
        // Seeded with None (cold start).
        assert!(rx.borrow().is_none(), "receiver must start empty");

        let snap = Snapshot::empty(Utc::now());
        sink.on_snapshot(&snap);

        let guard = rx.borrow();
        let published = guard.as_ref().expect("snapshot published");
        assert_eq!(published.schema_version, snap.schema_version);
    }

    #[test]
    fn channel_sink_on_degraded_is_noop() {
        let (mut sink, rx) = ChannelSink::new();
        sink.on_degraded(Source::OpenAiCosts, "boom");
        assert!(
            rx.borrow().is_none(),
            "on_degraded must not publish a snapshot"
        );
    }

    // -- TerminalGuard restore --------------------------------------------

    #[test]
    fn restore_terminal_is_idempotent() {
        // Restore must be safe to call when no guard ever entered raw mode
        // (e.g. panic hook fires in a non-TTY test process). It must not panic
        // and must be callable twice.
        restore_terminal();
        restore_terminal();
    }

    // -- classify_key ------------------------------------------------------

    #[test]
    fn classify_key_maps_quit_refresh_ignore() {
        assert_eq!(
            classify_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            Action::Quit
        );
        assert_eq!(
            classify_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Action::Quit
        );
        assert_eq!(
            classify_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
        assert_eq!(
            classify_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
            Action::Refresh
        );
        assert_eq!(
            classify_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Action::Ignore
        );
    }

    #[test]
    fn classify_key_ignores_release_and_repeat() {
        // Windows emits a Release (and Repeat) for every keystroke; acting on
        // them would double-fire `r`. Only Press is honored.
        assert_eq!(
            classify_key(KeyEvent::new_with_kind(
                KeyCode::Char('r'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            )),
            Action::Ignore,
            "a Release event must not trigger Refresh"
        );
        assert_eq!(
            classify_key(KeyEvent::new_with_kind(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            )),
            Action::Ignore,
            "a Repeat event must not trigger Quit"
        );
    }

    // -- draw_ui goldens ---------------------------------------------------

    /// Flatten a TestBackend buffer into a single string for substring asserts.
    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    /// Minimal `OpenAiCosts` with empty line items; we only render the total.
    fn openai_zero_costs() -> OpenAiCosts {
        OpenAiCosts {
            total_micro_usd: 0,
            start_time: ts("2026-05-01T00:00:00Z"),
            end_time: ts("2026-05-15T00:00:00Z"),
            by_line_item: Vec::new(),
            truncated: false,
            fetched_at: ts("2026-05-15T11:02:00Z"),
        }
    }

    fn populated_snapshot() -> Snapshot {
        let now = ts("2026-05-15T11:02:00Z");
        let mut snap = Snapshot::empty(now);
        snap.claude_oauth = Some(ClaudeOAuthSnapshot {
            cadences: vec![
                CadenceBar {
                    key: "five_hour".to_string(),
                    display_label: "Current 5-hour session".to_string(),
                    utilization_percent: 42.5,
                    resets_at: ts("2026-05-15T13:00:00Z"),
                },
                CadenceBar {
                    key: "seven_day".to_string(),
                    display_label: "All models (7 days)".to_string(),
                    utilization_percent: 18.3,
                    resets_at: ts("2026-05-20T00:00:00Z"),
                },
            ],
            extra_usage: Some(ExtraUsage {
                is_enabled: true,
                monthly_limit_micro_usd: 25_000_000,
                used_credits_micro_usd: 20_920_000,
                utilization_percent: 83.7,
                currency: "USD".to_string(),
            }),
            subscription_type: Some("max".to_string()),
            rate_limit_tier: None,
            org_uuid: None,
            fetched_at: now,
        });
        snap.codex_quota = Some(CodexQuotaSnapshot {
            observed_at: ts("2026-05-15T10:55:00Z"),
            session_id: "sess-1".to_string(),
            primary: RateLimitWindow {
                used_percent: 17.5,
                window_duration_minutes: 10080,
                resets_at: ts("2026-05-22T10:55:00Z"),
            },
            secondary: None,
            plan_type: "go".to_string(),
            rate_limit_reached: false,
        });
        snap.openai = Some(OpenAiCosts {
            total_micro_usd: 4_237_000,
            ..openai_zero_costs()
        });
        snap
    }

    fn render_to_terminal(snap: &Snapshot) -> Terminal<TestBackend> {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_ui(f, snap)).unwrap();
        terminal
    }

    #[test]
    fn tui_render_populated_shows_quota_and_dollars() {
        let snap = populated_snapshot();
        let terminal = render_to_terminal(&snap);
        let text = buffer_text(&terminal);
        assert!(text.contains("5h"), "missing 5h label in:\n{text}");
        assert!(text.contains("7d"), "missing 7d label in:\n{text}");
        assert!(text.contains("42"), "missing 5h percent 42(.5) in:\n{text}");
        assert!(text.contains("Codex"), "missing Codex block in:\n{text}");
        // Admin costs dollar formatting via micro_usd_to_display_dollars.
        assert!(text.contains("$4.24"), "missing admin $ in:\n{text}");
        // Extra-usage overage $ (real billed).
        assert!(text.contains("$20.92"), "missing extra_usage $ in:\n{text}");
        // Keybind footer.
        assert!(text.contains("quit"), "missing keybind footer in:\n{text}");
        assert!(
            text.contains("refresh"),
            "missing refresh keybind in:\n{text}"
        );
    }

    #[test]
    fn tui_render_degraded_shows_banner() {
        let mut snap = populated_snapshot();
        snap.openai = None;
        snap.openai_error = Some("admin costs fetch failed".to_string());
        let terminal = render_to_terminal(&snap);
        let text = buffer_text(&terminal);
        assert!(
            text.to_uppercase().contains("DEGRADED"),
            "degraded banner missing in:\n{text}"
        );
        assert!(
            text.contains("OpenAiCosts") || text.contains("OpenAI"),
            "degraded banner must name the failing source in:\n{text}"
        );
    }

    #[test]
    fn tui_render_cold_start_does_not_panic() {
        let snap = Snapshot::empty(ts("2026-05-15T11:02:00Z"));
        let terminal = render_to_terminal(&snap);
        let text = buffer_text(&terminal);
        // Cold start must render *something* glanceable, not a blank frame.
        assert!(
            text.contains("Balanze"),
            "cold-start frame missing title in:\n{text}"
        );
    }
}
