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

    // Monitor under the cursor gives clamp bounds (full monitor, not work area
    // — fine because we anchor ABOVE the cursor, above a bottom taskbar).
    let monitor = match app.monitor_from_point(cursor.x, cursor.y) {
        Ok(Some(m)) => m,
        Ok(None) => {
            tracing::warn!("popover anchor: no monitor under cursor; showing unanchored");
            position_and_show(app);
            return;
        }
        Err(e) => {
            tracing::warn!("popover anchor: monitor_from_point failed ({e}); showing unanchored");
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

    // Bottom-right corner at the cursor.
    let mut x = cursor.x as i32 - win_w;
    let mut y = cursor.y as i32 - win_h;

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

    let settings = settings::load().unwrap_or_else(|e| {
        tracing::warn!("settings load failed ({e}); using defaults");
        settings::Settings::default()
    });

    let sink = TauriSink::new(app.handle().clone());
    let (handle, coord_join) = state_coordinator::spawn(sink);
    app.manage(handle.clone());

    let watcher_handles = watcher::Watcher::spawn(handle, &settings);

    rt.spawn(async move {
        for (label, h) in watcher_handles {
            tokio::spawn(async move {
                match h.await {
                    Ok(Ok(())) => tracing::debug!("watcher/{label}: exited Ok(())"),
                    Ok(Err(e)) => tracing::error!("watcher/{label}: returned error: {e}"),
                    Err(je) => tracing::error!("watcher/{label}: panicked/aborted: {je}"),
                }
            });
        }
        match coord_join.await {
            Ok(()) => tracing::error!("state_coordinator task exited"),
            Err(je) => tracing::error!("state_coordinator panicked/aborted: {je}"),
        }
    });
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
            commands::get_settings,
            commands::set_settings,
            commands::set_api_key
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
