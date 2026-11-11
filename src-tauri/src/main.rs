//! fastPaste entry point: single instance, tray, global hotkey, panel window.
//! Close button / X hides the panel to tray (QQ-style); only tray menu 退出
//! actually quits the app. V2.0 constraints: no DB process, no bundled
//! Chromium, no non-user-initiated network.
#![cfg(feature = "app")]
use fastpaste_lib::commands::{self, AppState};
use fastpaste_lib::monitor::{MonitorCmd, MonitorHandle};
use fastpaste_lib::store::Store;
use parking_lot::Mutex;
use std::sync::Arc;
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

fn data_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(std::path::PathBuf::from)
        .unwrap_or_default().join("Library/Application Support");
    #[cfg(not(target_os = "macos"))]
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("fastPaste")
}

fn main() {
    // Single instance (2.1): second launch signals the running one and exits.
    if std::net::TcpListener::bind("127.0.0.1:46371").is_err() {
        let _ = std::net::TcpStream::connect("127.0.0.1:46371");
        return;
    }

    let dir = data_dir();
    std::fs::create_dir_all(&dir).expect("create data dir");
    let mut store = Store::open(&dir).expect("open store");
    store.enforce_retention().ok();
    let config = store.config.clone();
    let store = Arc::new(Mutex::new(store));

    let MonitorHandle { cmd: monitor_cmd, manual_only } = MonitorHandle::new(store.clone(), config.paste_aware);
    let events_rx = fastpaste_lib::monitor::take_events();

    let state = AppState {
        store,
        log: Mutex::new(vec![]),
        monitor: MonitorHandle { cmd: monitor_cmd.clone(), manual_only },
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(state)
        .setup(move |app| {
            commands::forward_events(app.handle().clone(), events_rx);

            // QQ-style close: the X button hides the panel instead of quitting.
            // Only the tray menu 退出 terminates the app.
            if let Some(panel) = app.get_webview_window("panel") {
                let panel_clone = panel.clone();
                panel.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = panel_clone.hide();
                    }
                });
            }

            // FR-B02: honor auto-launch setting at startup.
            if config.auto_launch {
                let _ = app.auto_launch().enable();
            }

            // FR-B01: tray / menu-bar icon.
            let open = tauri::menu::MenuItem::with_id(app, "open", "打开面板", true, None::<&str>)?;
            let pause = tauri::menu::MenuItem::with_id(app, "pause", "暂停记录", true, None::<&str>)?;
            let settings = tauri::menu::MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?;
            let quit = tauri::menu::MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = tauri::menu::MenuBuilder::new(app)
                .items(&[&open, &pause, &settings, &quit])
                .build()?;
            let _tray = tauri::tray::TrayIconBuilder::new()
                .tooltip("fastPaste")
                .icon(app.default_window_icon().unwrap().clone())
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click { button: tauri::tray::MouseButton::Left, .. } = event {
                        commands::show_panel_cmd(tray.app_handle());
                    }
                })
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => commands::show_panel_cmd(app),
                    "settings" => {
                        commands::show_panel_cmd(app);
                        let _ = app.emit_to("panel", "open-settings", ());
                    }
                    "pause" => {
                        if let Some(st) = app.try_state::<AppState>() {
                            let mut g = st.store.lock();
                            g.config.pause_recording = !g.config.pause_recording;
                            let v = g.config.pause_recording;
                            drop(g);
                            st.log(format!("recording paused: {}", v));
                        }
                    }
                    "quit" => {
                        // The ONLY exit path: right-click tray -> 退出 (QQ-style).
                        if let Some(st) = app.try_state::<AppState>() {
                            let _ = st.monitor.cmd.send(MonitorCmd::Shutdown);
                            st.store.lock().compact().ok();
                        }
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            // FR-K01: global hotkey; conflict -> refuse registration & log.
            if let Err(e) = commands::register_hotkey(app.handle(), &config.hotkey) {
                eprintln!("hotkey registration failed (conflict?): {}", e);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::search, commands::recent, commands::delete_item, commands::pin_item,
            commands::pause_recording, commands::get_config, commands::set_config,
            commands::add_exclude, commands::remove_exclude, commands::clear_all,
            commands::export_archive, commands::import_archive, commands::diagnostics,
            commands::item_text, commands::paste_item, commands::paste_stack, commands::copy_item,
            commands::set_auto_launch, commands::update_hotkey,
        ])
        .run(tauri::generate_context!())
        .expect("error while running fastPaste");
}
