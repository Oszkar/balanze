use tauri::{
    App, Manager,
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem},
    tray::TrayIconEvent,
};

// Sink-seam checkpoint — see `tauri_sink` module docs. Skeleton only; v0.3
// wires it into the state_coordinator actor.
mod tauri_sink;

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn setup_tray(app: &mut App) -> tauri::Result<()> {
    let open = MenuItemBuilder::with_id("open", "Open Balanze").build(app)?;
    let settings = MenuItemBuilder::with_id("settings", "Settings\u{2026}").build(app)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;

    let menu = MenuBuilder::new(app)
        .items(&[&open, &settings, &separator, &quit])
        .build()?;

    let tray = app
        .tray_by_id("main")
        .expect("tray 'main' is declared in tauri.conf.json");

    tray.set_menu(Some(menu))?;
    tray.on_menu_event(|app, event| match event.id().as_ref() {
        "open" => show_main_window(app),
        "settings" => {
            // TODO: open settings window when it exists; for v0.1 spike, reuse main window.
            show_main_window(app);
        }
        "quit" => app.exit(0),
        _ => {}
    });

    tray.on_tray_icon_event(|tray, event| {
        if let TrayIconEvent::Click { .. } = event {
            show_main_window(tray.app_handle());
        }
    });

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            setup_tray(app)?;
            Ok(())
        })
        // No commands registered: the documented IPC contract (AGENTS.md §4 #9)
        // — get_snapshot, get_history, refresh_now, set_api_key, get_settings,
        // set_settings — is v0.3 work. Until then the Tauri surface is the tray
        // menu only; no command is exposed to the webview.
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
