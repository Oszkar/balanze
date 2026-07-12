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
    // if the window is wider/taller than the monitor; `max(left, ...)` keeps the
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

/// Pure geometry for the post-resize re-anchor: given the window's top-left, its
/// pre-resize outer height, the new outer size, and the monitor bounds, return
/// the clamped new top-left. Split out from [`reanchor_after_resize`] so both the
/// platform branch and the on-monitor clamp are unit-testable without a live
/// window (AGENTS.md §6/§7).
///
/// `pin_bottom` selects the fixed edge. `true` (Windows/Linux, popover opens up
/// from the taskbar) pins the OLD bottom edge - the pre-resize top (`pos_y`,
/// unchanged by `set_size`) plus the OLD height - and derives the new top from
/// the new height, so the window grows upward instead of pushing its bottom
/// off-screen. `false` (macOS, popover drops from the menu bar) keeps the top
/// edge where the drop placed it. Either way the result is clamped fully
/// on-monitor.
#[allow(clippy::too_many_arguments)]
fn reanchored_position(
    pin_bottom: bool,
    pos_x: i32,
    pos_y: i32,
    old_outer_h: u32,
    win_w: i32,
    win_h: i32,
    mon_left: i32,
    mon_top: i32,
    mon_right: i32,
    mon_bottom: i32,
) -> (i32, i32) {
    let x = pos_x;
    let y = if pin_bottom {
        (pos_y + old_outer_h as i32) - win_h
    } else {
        pos_y
    };

    // Clamp fully on-monitor. `max_x`/`max_y` can fall below the monitor origin
    // if the window is wider/taller than the monitor; `max(left/top, ...)` keeps
    // the top-left corner on-screen in that degenerate case.
    let max_x = (mon_right - win_w).max(mon_left);
    let max_y = (mon_bottom - win_h).max(mon_top);
    (x.clamp(mon_left, max_x), y.clamp(mon_top, max_y))
}

/// Re-anchor the popover after a content resize, reading live position/monitor
/// geometry and applying [`reanchored_position`]. The new size is passed in by
/// the resize command because OS window managers may not report the post-resize
/// outer size synchronously after `set_size`. macOS keeps the top edge fixed
/// (menu-bar drop); Windows/Linux keep the bottom edge fixed (taskbar pop-up).
fn reanchor_after_resize(
    window: &tauri::WebviewWindow,
    old_outer_h: u32,
    new_outer: tauri::PhysicalSize<u32>,
) -> tauri::Result<()> {
    let pos = window.outer_position()?; // top-left unchanged by set_size
    let Some(monitor) = window.current_monitor()? else {
        return Ok(());
    };
    let mp = monitor.position();
    let ms = monitor.size();
    let (x, y) = reanchored_position(
        !cfg!(target_os = "macos"),
        pos.x,
        pos.y,
        old_outer_h,
        new_outer.width as i32,
        new_outer.height as i32,
        mp.x,
        mp.y,
        mp.x + ms.width as i32,
        mp.y + ms.height as i32,
    );
    window.set_position(tauri::PhysicalPosition::new(x, y))?;
    Ok(())
}

/// Suppresses the blur-to-hide auto-hide for a short window after the first-run
/// welcome auto-opens the popover. At startup (especially a slow dev launch) the
/// OS often moves foreground focus off the just-shown window, firing
/// `WindowEvent::Focused(false)` and hiding the popover before the user sees it.
/// Holds the `Instant` the welcome was shown; the blur-hide handler ignores the
/// hide while that is recent. Tray-click opens never set it, so normal
/// click-away dismiss is unaffected.
struct WelcomeGrace(std::sync::Mutex<Option<std::time::Instant>>);

/// How long the first-run welcome keeps the popover up despite blur: long enough
/// to survive the startup focus race, short enough that an un-engaged popover
/// does not linger. Shared by the blur-hide grace and the delayed auto-hide.
const FIRST_RUN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// On the very first launch (per the persisted `seen_welcome` flag), make the
/// app's presence obvious: open the popover once and fire an OS notification.
/// Without this the app starts as just a tray icon, easy to miss - especially in
/// the Windows hidden-overflow tray. Best-effort: every failure is logged, never
/// fatal, and the flag is persisted so the welcome shows exactly once.
fn maybe_first_run_welcome(app: &App, rt: &tokio::runtime::Handle) {
    let mut settings = match settings::load() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("first-run: settings load failed ({e}); skipping welcome");
            return;
        }
    };
    if settings.seen_welcome {
        return;
    }
    tracing::info!("first-run: showing welcome (popover + notification)");

    // Persist the flag FIRST, before showing. Saving the loaded struct after the
    // popover is up could clobber a settings write the just-opened popover makes
    // in between; nothing here mutates settings further, so writing it up front
    // is both safe and race-free.
    settings.seen_welcome = true;
    if let Err(e) = settings::save(&settings) {
        tracing::warn!("first-run: settings save failed ({e}); welcome may repeat next launch");
    }

    // Arm the blur-hide grace BEFORE showing: the startup focus race that
    // immediately follows show() must not hide the popover before it is seen.
    if let Some(grace) = app.try_state::<WelcomeGrace>() {
        if let Ok(mut shown) = grace.0.lock() {
            *shown = Some(std::time::Instant::now());
        }
    }
    // Open the popover unanchored - there is no tray click to anchor to at
    // startup. It shows the loading/empty state until the first poll lands.
    position_and_show(app.handle());
    notify_first_run(app);

    // After the grace window, auto-hide IF the popover never gained focus (the
    // race left it visible-but-unfocused, or the user never engaged). If it has
    // focus the user is looking at it - leave it, and the now-expired grace lets
    // normal click-away dismiss take over. Without this the popover could linger
    // until an explicit ESC/tray interaction. One-shot, not a supervised task.
    if let Some(window) = app.get_webview_window("main") {
        rt.spawn(async move {
            tokio::time::sleep(FIRST_RUN_GRACE).await;
            if !window.is_focused().unwrap_or(false) {
                let _ = window.hide();
            }
        });
    }
}

/// Fire the one-time "Balanze is running" OS notification, requesting the OS
/// notification permission first (macOS prompts; Windows is typically granted).
/// Fired from Rust, so it needs no webview capability (PoLP). Windows toasts
/// only surface for the installed app, not `tauri dev`.
fn notify_first_run(app: &App) {
    use tauri_plugin_notification::{NotificationExt, PermissionState};

    let notifier = app.notification();
    // Show only with permission: use the current grant, otherwise request it
    // once (macOS prompts; Windows is typically granted). request_permission is
    // only called when we don't already hold the grant.
    let granted = matches!(notifier.permission_state(), Ok(PermissionState::Granted))
        || matches!(notifier.request_permission(), Ok(PermissionState::Granted));
    if !granted {
        tracing::info!("first-run: notification permission not granted; skipping toast");
        return;
    }
    if let Err(e) = notifier
        .builder()
        .title("Balanze is running")
        .body("Balanze lives in your tray. Click the icon to see your Claude and OpenAI usage.")
        .show()
    {
        tracing::warn!("first-run: notification show failed ({e})");
    }
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
    let (handle, coord_join) = state_coordinator::spawn_with_optional_file(sink);
    app.manage(handle.clone());

    // Live settings-apply: settings/key commands signal a reload; the
    // supervisor re-spawns the watcher with fresh settings so provider toggles
    // take effect without an app restart (the coordinator clears disabled
    // cells; this starts/stops the actual polling).
    let (reload_tx, reload_rx) = tokio::sync::mpsc::channel::<commands::SettingsTransition>(8);
    app.manage(commands::WatcherReload(reload_tx));

    // The watcher supervisor is itself a long-running task (AGENTS.md §3.2). If it
    // panics, the reload + self-heal machinery is gone and the surviving watcher
    // tasks stop being supervised - the same silent death this PR closes, one level
    // up. So monitor it like the coordinator below: a genuine panic is fatal; a
    // clean return or a shutdown abort is benign.
    let supervisor_join = rt.spawn(supervise_watcher(handle, reload_rx));
    let supervisor_app = app.handle().clone();
    rt.spawn(async move {
        match supervisor_join.await {
            Ok(()) => tracing::info!("watcher supervisor stopped (shutdown)"),
            Err(je) if je.is_cancelled() => {
                tracing::info!("watcher supervisor aborted (shutdown)")
            }
            Err(je) => {
                tracing::error!("watcher supervisor panicked ({je}); exiting");
                supervisor_app.exit(1);
            }
        }
    });

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
    mut reload_rx: tokio::sync::mpsc::Receiver<commands::SettingsTransition>,
) {
    enum Wake {
        Reload(Box<commands::SettingsTransition>),
        Death(&'static str),
        Shutdown,
    }

    let mut consecutive_failures: u32 = 0;
    let mut last_failure: Option<std::time::Instant> = None;
    let mut generation: state_coordinator::WatcherGeneration = 1;
    let mut current_settings = settings::load_or_default();
    let mut watched = match activate_watcher_generation(&handle, &current_settings, generation)
        .await
    {
        Ok(watched) => watched,
        Err(error) => {
            tracing::error!("watcher supervisor failed to activate initial generation: {error}");
            return;
        }
    };

    loop {
        let wake = tokio::select! {
            r = reload_rx.recv() => r.map_or(Wake::Shutdown, |transition| Wake::Reload(Box::new(transition))),
            label = watched.recv_death() => label.map_or(Wake::Shutdown, Wake::Death),
        };

        match wake {
            Wake::Shutdown => {
                watched.shutdown().await;
                tracing::info!("watcher supervisor: settings channel closed; stopping");
                return;
            }
            Wake::Reload(transition) => {
                generation = match generation.checked_add(1) {
                    Some(next) => next,
                    None => {
                        let _ = transition
                            .applied
                            .send(Err("watcher generation counter exhausted".to_string()));
                        watched.shutdown().await;
                        return;
                    }
                };
                let settings = transition.settings;
                match replace_watcher_generation(watched, &handle, &settings, generation).await {
                    Ok(next) => {
                        watched = next;
                        current_settings = settings;
                        consecutive_failures = 0;
                        tracing::info!(
                            "watcher: settings transition to generation {generation} completed"
                        );
                        let _ = transition.applied.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = transition.applied.send(Err(error.clone()));
                        tracing::error!("watcher settings transition failed: {error}");
                        return;
                    }
                }
            }
            Wake::Death(label) => {
                // Surface the affected cell as degraded so the UI shows a warning
                // rather than silently-stale data. A successful re-spawn emits a
                // fresh Ok update that clears the error slot.
                if let Some(source) = watcher::source_for_label(label) {
                    let _ = handle
                        .send(state_coordinator::StateMsg::Update(
                            state_coordinator::SourceUpdate {
                                generation,
                                source,
                                result: Err(format!(
                                    "watcher task '{label}' stopped unexpectedly; retrying"
                                )),
                            },
                        ))
                        .await;
                }
                watched.shutdown().await;
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
                generation = match generation.checked_add(1) {
                    Some(next) => next,
                    None => {
                        tracing::error!("watcher generation counter exhausted");
                        return;
                    }
                };
                watched = match activate_watcher_generation(&handle, &current_settings, generation)
                    .await
                {
                    Ok(next) => next,
                    Err(error) => {
                        tracing::error!("watcher restart failed: {error}");
                        return;
                    }
                };
            }
        }
    }
}

async fn replace_watcher_generation(
    current: watcher::WatchedTasks,
    handle: &state_coordinator::StateCoordinatorHandle,
    settings: &settings::Settings,
    generation: state_coordinator::WatcherGeneration,
) -> Result<watcher::WatchedTasks, String> {
    current.shutdown().await;
    activate_watcher_generation(handle, settings, generation).await
}

async fn activate_watcher_generation(
    handle: &state_coordinator::StateCoordinatorHandle,
    settings: &settings::Settings,
    generation: state_coordinator::WatcherGeneration,
) -> Result<watcher::WatchedTasks, String> {
    let (applied, confirmed) = tokio::sync::oneshot::channel();
    handle
        .send(state_coordinator::StateMsg::SettingsChanged {
            settings: Box::new(settings.clone()),
            generation,
            applied,
        })
        .await
        .map_err(|_| "state coordinator has shut down".to_string())?;
    confirmed
        .await
        .map_err(|_| "state coordinator dropped settings acknowledgment".to_string())?;

    let tasks = watcher::Watcher::spawn(handle.clone(), settings, generation);
    tracing::info!(
        "watcher: spawned {} task(s) for generation {generation}",
        tasks.len()
    );
    Ok(watcher::watch_for_task_death(tasks))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Held for the process lifetime (see `logging::init_tracing` doc comment).
    let _log_guard = logging::init_tracing("balanze-gui", true);

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
        .plugin(tauri_plugin_notification::init())
        .manage(rt)
        .manage(WelcomeGrace(std::sync::Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::refresh_now,
            commands::hide_window,
            commands::resize_popover,
            commands::get_settings,
            commands::set_settings,
            commands::set_api_key,
            commands::validate_api_key,
            commands::has_api_key,
            commands::clear_api_key,
            commands::get_statusline_status,
            commands::set_statusline_wired,
            commands::replace_statusline,
            commands::restore_statusline
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::Focused(false) = event {
                // Ignore the blur-to-hide during the first-run welcome grace
                // window: at startup the OS often moves focus off the just-shown
                // popover, firing Focused(false) before the user sees it. Normal
                // tray opens never arm WelcomeGrace, so click-away dismiss is
                // unaffected.
                let in_grace = window
                    .try_state::<WelcomeGrace>()
                    .and_then(|g| g.0.lock().ok().and_then(|shown| *shown))
                    .is_some_and(|t| t.elapsed() < FIRST_RUN_GRACE);
                if in_grace {
                    return;
                }
                let _ = window.hide();
            }
        })
        .setup(move |app| {
            setup_tray(app)?;
            boot_backend(app, &rt_handle);
            maybe_first_run_welcome(app, &rt_handle);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{reanchored_position, replace_watcher_generation, respawn_backoff};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct StopFlag(Arc<AtomicBool>);

    impl Drop for StopFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn settings_transition_joins_old_generation_before_completion() {
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = StopFlag(Arc::clone(&stopped));
        let old_task = tokio::spawn(async move {
            let _flag = flag;
            std::future::pending::<Result<(), watcher::WatcherError>>().await
        });
        let old = watcher::watch_for_task_death(vec![("test", old_task)]);
        let (handle, coordinator) = state_coordinator::spawn(state_coordinator::NullSink);
        let settings = settings::Settings {
            providers: settings::ProviderSettings {
                anthropic_enabled: false,
                openai_enabled: false,
                codex_enabled: false,
            },
            ..settings::Settings::default()
        };

        let next = replace_watcher_generation(old, &handle, &settings, 1)
            .await
            .unwrap();
        assert!(
            stopped.load(Ordering::SeqCst),
            "transition completion must follow old-generation join"
        );

        next.shutdown().await;
        drop(handle);
        coordinator.await.unwrap();
    }

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

    // A roomy 1920x1080 monitor at the origin, so the clamp is a no-op and these
    // cases isolate the platform branch. Window at (100, 50), old height 560.
    #[test]
    fn reanchor_macos_pins_top_edge() {
        // pin_bottom = false: y stays at the menu-bar drop position regardless of
        // the height delta; x is unchanged.
        let (x, y) = reanchored_position(false, 100, 50, 560, 360, 600, 0, 0, 1920, 1080);
        assert_eq!((x, y), (100, 50));
    }

    #[test]
    fn reanchor_windows_pins_bottom_edge() {
        // pin_bottom = true: the OLD bottom (pos_y + old_h = 50 + 560 = 610) stays
        // put. Growing 560 -> 600 lifts the top to 610 - 600 = 10.
        let (_, y_grow) = reanchored_position(true, 100, 50, 560, 360, 600, 0, 0, 1920, 1080);
        assert_eq!(y_grow, 10);
        // Shrinking 560 -> 400 drops the top back to 610 - 400 = 210.
        let (_, y_shrink) = reanchored_position(true, 100, 50, 560, 360, 400, 0, 0, 1920, 1080);
        assert_eq!(y_shrink, 210);
    }

    #[test]
    fn reanchor_clamps_top_into_monitor() {
        // A bottom-pin tall enough to push the top above the monitor origin
        // (610 - 900 = -290) is clamped to mon_top.
        let (_, y) = reanchored_position(true, 100, 50, 560, 360, 900, 0, 0, 1920, 1080);
        assert_eq!(y, 0);
    }

    #[test]
    fn reanchor_clamps_x_to_right_edge() {
        // x past the right edge is pulled back so the window stays fully
        // on-monitor: max_x = 1920 - 360 = 1560.
        let (x, _) = reanchored_position(false, 1800, 50, 560, 360, 600, 0, 0, 1920, 1080);
        assert_eq!(x, 1560);
    }

    #[test]
    fn reanchor_oversized_window_keeps_top_left_on_screen() {
        // A window larger than the monitor has no fully-on-screen placement;
        // max_x/max_y fall back to the origin so the top-left stays visible.
        let (x, y) = reanchored_position(false, 5000, 5000, 560, 4000, 4000, 0, 0, 1920, 1080);
        assert_eq!((x, y), (0, 0));
    }
}
