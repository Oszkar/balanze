use tauri::{
    App, Manager, PhysicalPosition, WindowEvent,
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconEvent},
};

mod commands;
mod tauri_sink;
mod tray_icon;

use tauri_sink::TauriSink;

/// Ask the coordinator to re-emit the current snapshot via `usage_updated`.
///
/// Every successful show path calls this so the popover reflects the latest
/// state even if its webview listener missed a live emit (the OpenAI-only
/// startup race). `try_send` is sync + non-blocking; a full channel or an
/// absent coordinator (boot ordering, headless tests) is a no-op, not an error.
fn refresh_state_on_open(app: &tauri::AppHandle) {
    if let Some(handle) = app.try_state::<state_coordinator::StateCoordinatorHandle>() {
        let _ = handle.try_send(state_coordinator::StateMsg::Refresh);
    }
}

fn position_and_show(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        refresh_state_on_open(app);
    }
}

/// Show the popover anchored at the current cursor, for open paths that have
/// no click event of their own (the tray menu item and the single-instance
/// callback). Falls back to the unanchored path if the cursor is unavailable.
fn show_popover_at_cursor(app: &tauri::AppHandle) {
    match app.cursor_position() {
        Ok(p) => show_popover_anchored(app, p),
        Err(e) => {
            tracing::warn!("cursor_position failed ({e}); showing unanchored");
            position_and_show(app);
        }
    }
}

/// Show the popover anchored next to the tray icon, fully on-screen.
///
/// `cursor` is the click position (physical px) carried by
/// `TrayIconEvent::Click`. We place the window's bottom-right corner at the
/// cursor (the common Windows bottom-right taskbar case) and clamp to the
/// monitor under the cursor so it never lands off-screen. Anything that can
/// fail (monitor/size lookups) degrades to the unanchored show path; no panics.
fn show_popover_anchored(app: &tauri::AppHandle, cursor: PhysicalPosition<f64>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    let win = match window.outer_size() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("popover anchor: outer_size failed ({e}); showing unanchored");
            position_and_show(app);
            return;
        }
    };

    // Find the monitor under the (physical) cursor by testing each monitor's
    // physical bounds directly. We do NOT use `monitor_from_point`: on macOS it
    // treats the point as logical, so a physical tray-click coordinate on a
    // Retina display (e.g. 2222,32) lands off-screen and returns None, sending
    // the popover to the unanchored fallback. Falls back to the primary monitor
    // (where the macOS menu bar / tray lives) if no monitor contains the point.
    let cx = cursor.x as i32;
    let cy = cursor.y as i32;
    let monitor = app
        .available_monitors()
        .ok()
        .and_then(|mons| {
            mons.into_iter().find(|m| {
                let p = m.position();
                let s = m.size();
                cx >= p.x && cx < p.x + s.width as i32 && cy >= p.y && cy < p.y + s.height as i32
            })
        })
        .or_else(|| app.primary_monitor().ok().flatten());
    let monitor = match monitor {
        Some(m) => m,
        None => {
            tracing::warn!("popover anchor: no monitor found; showing unanchored");
            position_and_show(app);
            return;
        }
    };

    let mon_pos = monitor.position();
    let mon_size = monitor.size();
    let win_w = win.width as i32;
    let win_h = win.height as i32;

    let mon_left = mon_pos.x;
    let mon_top = mon_pos.y;
    let mon_right = mon_pos.x + mon_size.width as i32;
    let mon_bottom = mon_pos.y + mon_size.height as i32;

    // Anchor differently per platform. macOS has a top menu bar (the tray icon
    // lives there), so the popover drops DOWN from the bar: centered under the
    // click (~ the icon), top edge at the work-area top - i.e. just below the
    // menu bar. `work_area` excludes the menu bar, so its top is the correct
    // dock line regardless of bar height (notch displays) or DPI scaling, which
    // a fixed pixel offset from the cursor got wrong. Windows' tray is on a
    // bottom taskbar, so the window's bottom-right corner anchors at the cursor
    // (opens up-and-left).
    #[cfg(target_os = "macos")]
    let (mut x, mut y) = (cursor.x as i32 - win_w / 2, monitor.work_area().position.y);
    #[cfg(not(target_os = "macos"))]
    let (mut x, mut y) = (cursor.x as i32 - win_w, cursor.y as i32 - win_h);

    // Clamp fully on-monitor. `max_x`/`max_y` can fall below the monitor origin
    // if the window is wider/taller than the monitor; `max(left, …)` keeps the
    // top-left corner on-screen in that degenerate case.
    let max_x = (mon_right - win_w).max(mon_left);
    let max_y = (mon_bottom - win_h).max(mon_top);
    x = x.clamp(mon_left, max_x);
    y = y.clamp(mon_top, max_y);

    if let Err(e) = window.set_position(PhysicalPosition::new(x, y)) {
        tracing::warn!("popover anchor: set_position failed ({e}); showing unanchored");
        position_and_show(app);
        return;
    }
    let _ = window.show();
    let _ = window.set_focus();
    refresh_state_on_open(app);
}

fn setup_tray(app: &mut App) -> tauri::Result<()> {
    let open = MenuItemBuilder::with_id("open", "Open Balanze").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[&open, &PredefinedMenuItem::separator(app)?, &quit])
        .build()?;

    let tray = app
        .tray_by_id("main")
        .expect("tray 'main' is declared in tauri.conf.json");
    tray.set_menu(Some(menu))?;
    tray.on_menu_event(|app, event| match event.id().as_ref() {
        "open" => show_popover_at_cursor(app),
        "quit" => app.exit(0),
        _ => {}
    });
    tray.on_tray_icon_event(|tray, event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            position,
            ..
        } = event
        {
            show_popover_anchored(tray.app_handle(), position);
        }
    });
    Ok(())
}

/// Boot the live data spine: coordinator + TauriSink + watcher, supervised.
/// Mirrors `balanze_cli::watch_cmd::run_with_sink` (AGENTS.md §4 #4/#7).
fn boot_backend(app: &App, rt: &tokio::runtime::Handle) {
    let _enter = rt.enter();

    let sink = TauriSink::new(app.handle().clone());
    let (handle, coord_join) = state_coordinator::spawn(sink);
    app.manage(handle.clone());

    // Live settings-apply: settings/key commands signal a reload; the
    // supervisor re-spawns the watcher with fresh settings so provider toggles
    // take effect without an app restart (the coordinator clears disabled
    // cells; this starts/stops the actual polling).
    let (reload_tx, reload_rx) = tokio::sync::mpsc::channel::<()>(8);
    app.manage(commands::WatcherReload(reload_tx));

    rt.spawn(supervise_watcher(handle, reload_rx));

    // The coordinator is the actor that owns the Snapshot and the only tray/IPC
    // sink, so its death is fatal: without it the app is a frozen zombie. A
    // genuine panic exits the process (matching `balanze-cli --watch` and
    // AGENTS.md §3.2) so the user - and the single-instance relaunch - can
    // recover. A clean stop (Ok) or a cancellation (the runtime being torn down
    // on app exit) is normal shutdown and must NOT trigger exit(1).
    let coord_app = app.handle().clone();
    rt.spawn(async move {
        match coord_join.await {
            Ok(()) => {
                tracing::info!("state_coordinator stopped (all handles dropped; shutdown)")
            }
            Err(je) if je.is_cancelled() => {
                tracing::info!("state_coordinator aborted (shutdown)")
            }
            Err(je) => {
                tracing::error!("state_coordinator panicked ({je}); exiting");
                coord_app.exit(1);
            }
        }
    });
}

/// Re-spawn backoff after an unexpected watcher-task death: exponential from 1s,
/// capped at 60s, by consecutive-failure count. Bounds a persistent failure
/// (e.g. notify-exhaustion) to one retry per minute instead of a tight loop.
fn respawn_backoff(consecutive_failures: u32) -> std::time::Duration {
    let secs = 1u64
        .checked_shl(consecutive_failures)
        .unwrap_or(u64::MAX)
        .min(60);
    std::time::Duration::from_secs(secs)
}

/// If a task ran healthy at least this long before dying, treat the next death
/// as a fresh streak (reset the backoff) rather than escalating from the last.
const FAILURE_RESET_WINDOW: std::time::Duration = std::time::Duration::from_secs(300);

/// Own the watcher's task lifecycle. Re-spawns the task set on two triggers:
/// a settings reload (fresh settings, so provider toggles apply live) and an
/// *unexpected* task death (an `Err` return or panic). On a death it surfaces a
/// `degraded_state` for the affected source, then re-spawns with bounded backoff
/// so a persistent failure self-heals (or stays visibly degraded) instead of
/// silently freezing the source - the gap this closes. A clean `Ok(())` exit
/// (e.g. no Claude dir / no OpenAI key) and our own reload abort (a
/// cancellation) are NOT failures. Returns when the reload sender drops at app
/// shutdown.
async fn supervise_watcher(
    handle: state_coordinator::StateCoordinatorHandle,
    mut reload_rx: tokio::sync::mpsc::Receiver<()>,
) {
    enum Wake {
        Reload,
        Death(&'static str),
        Shutdown,
    }

    let mut consecutive_failures: u32 = 0;
    let mut last_failure: Option<std::time::Instant> = None;

    loop {
        let settings = settings::load().unwrap_or_else(|e| {
            tracing::warn!("settings load failed ({e}); using defaults");
            settings::Settings::default()
        });
        let tasks = watcher::Watcher::spawn(handle.clone(), &settings);
        tracing::info!("watcher: spawned {} task(s)", tasks.len());

        // Per-task watchdog: signal an *unexpected* completion (Err return or
        // panic) of any task through one channel, so the select! below learns of
        // a failure. Clean Ok(()) exits and our own reload aborts (cancellation)
        // are NOT signalled. Mirrors balanze_cli::watch_cmd. `death_tx` stays in
        // scope for the iteration, so `death_rx.recv()` only resolves on a real
        // death, never spuriously on all-senders-dropped.
        let (death_tx, mut death_rx) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
        let mut aborts = Vec::with_capacity(tasks.len());
        for (label, h) in tasks {
            aborts.push(h.abort_handle());
            let death_tx = death_tx.clone();
            tokio::spawn(async move {
                match h.await {
                    Ok(Ok(())) => tracing::debug!("watcher/{label}: exited Ok(())"),
                    Ok(Err(e)) => {
                        tracing::error!("watcher/{label}: returned error: {e}");
                        let _ = death_tx.send(label);
                    }
                    Err(je) if je.is_cancelled() => {}
                    Err(je) => {
                        tracing::error!("watcher/{label}: panicked/aborted: {je}");
                        let _ = death_tx.send(label);
                    }
                }
            });
        }

        let wake = tokio::select! {
            r = reload_rx.recv() => r.map_or(Wake::Shutdown, |()| Wake::Reload),
            Some(label) = death_rx.recv() => Wake::Death(label),
        };

        // Tear down the current set before the next action. The reload abort
        // shows up as a cancellation in each watchdog, which is ignored above.
        for a in aborts {
            a.abort();
        }

        match wake {
            Wake::Shutdown => {
                tracing::info!("watcher supervisor: reload channel closed; stopping");
                return;
            }
            Wake::Reload => {
                while reload_rx.try_recv().is_ok() {} // coalesce a burst
                consecutive_failures = 0; // user-initiated; not a failure streak
                tracing::info!("watcher: settings changed; re-spawning tasks");
            }
            Wake::Death(label) => {
                // Surface the affected cell as degraded so the UI shows a warning
                // rather than silently-stale data. A successful re-spawn emits a
                // fresh Ok update that clears the error slot.
                if let Some(source) = watcher::source_for_label(label) {
                    let _ = handle
                        .send(state_coordinator::StateMsg::Update(
                            state_coordinator::SourceUpdate {
                                source,
                                result: Err(format!(
                                    "watcher task '{label}' stopped unexpectedly; retrying"
                                )),
                            },
                        ))
                        .await;
                }
                // Fresh streak if it ran healthy for a while; else escalate.
                if last_failure.is_some_and(|t| t.elapsed() > FAILURE_RESET_WINDOW) {
                    consecutive_failures = 0;
                }
                last_failure = Some(std::time::Instant::now());
                let delay = respawn_backoff(consecutive_failures);
                consecutive_failures = consecutive_failures.saturating_add(1);
                tracing::warn!(
                    "watcher: task '{label}' died; re-spawning in {}s",
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // keyring-core has no default credential store until one is registered;
    // do it once here before the watcher (booted in `setup`) reads the key.
    keychain::init_default_store();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let rt_handle = rt.handle().clone();

    let mut builder = tauri::Builder::default();
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_popover_at_cursor(app);
        }));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .manage(rt)
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::refresh_now,
            commands::hide_window,
            commands::get_settings,
            commands::set_settings,
            commands::set_api_key,
            commands::has_api_key,
            commands::clear_api_key,
            commands::get_statusline_status,
            commands::set_statusline_wired
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::Focused(false) = event {
                let _ = window.hide();
            }
        })
        .setup(move |app| {
            setup_tray(app)?;
            boot_backend(app, &rt_handle);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::respawn_backoff;

    #[test]
    fn respawn_backoff_is_exponential_capped_at_60s() {
        assert_eq!(respawn_backoff(0).as_secs(), 1);
        assert_eq!(respawn_backoff(1).as_secs(), 2);
        assert_eq!(respawn_backoff(5).as_secs(), 32);
        assert_eq!(respawn_backoff(6).as_secs(), 60);
        // Large shifts must saturate to the cap, never panic on overflow.
        assert_eq!(respawn_backoff(64).as_secs(), 60);
        assert_eq!(respawn_backoff(u32::MAX).as_secs(), 60);
    }
}
