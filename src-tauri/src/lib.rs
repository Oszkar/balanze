use tauri::{
    App, Manager, WindowEvent,
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconEvent},
};

mod commands;
mod tauri_sink;
mod tray_icon;

use tauri_sink::TauriSink;

fn position_and_show(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
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
        "open" => position_and_show(app),
        "quit" => app.exit(0),
        _ => {}
    });
    tray.on_tray_icon_event(|tray, event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            position_and_show(tray.app_handle());
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

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let rt_handle = rt.handle().clone();

    let mut builder = tauri::Builder::default();
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            position_and_show(app);
        }));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .manage(rt)
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::refresh_now
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
