//! fastPaste entry point: single instance, tray, global hotkey, panel window.
//! V2.0 constraints honored here: no DB process, no bundled Chromium,
//! no non-user-initiated network.
use fastpaste_lib::commands::{self, AppState};
use fastpaste_lib::monitor::{MonitorCmd, MonitorHandle};
use fastpaste_lib::paste::Paster;
use fastpaste_lib::store::Store;
use parking_lot::Mutex;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::GlobalShortcutExt;

fn data_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(std::path::PathBuf::from)
        .unwrap_or_default().join("Library/Application Support");
    #[cfg(not(target_os = "macos"))]
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("fastPaste")
}

fn show_panel(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("panel") {
        let _ = w.center();
        let _ = w.show();
        let _ = w.set_focus();
        let _ = w.emit("panel-shown", ());
    }
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
        paster: Mutex::new(Paster::new()),
        log: Mutex::new(vec![]),
        monitor: MonitorHandle { cmd: monitor_cmd.clone(), manual_only },
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(state)
        .setup(move |app| {
            commands::forward_events(app.handle().clone(), events_rx);

            // FR-B01: tray / menu-bar icon.
            let _tray = tauri::tray::TrayIconBuilder::new()
                .tooltip("fastPaste")
                .icon(app.default_window_icon().unwrap().clone())
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click { button: tauri::tray::MouseButton::Left, .. } = event {
                        show_panel(tray.app_handle());
                    }
                })
                .menu(&tauri::menu::MenuBuilder::new(app).items(&[
                    &tauri::menu::MenuItem::with_id(app, "open", "打开面板", true, None::<&str>)?,
                    &tauri::menu::MenuItem::with_id(app, "pause", "暂停记录", true, None::<&str>)?,
                    &tauri::menu::MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?,
                    &tauri::menu::MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?,
                ])?.build()?)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_panel(app),
                    "settings" => {
                        show_panel(app);
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
            let hk = config.hotkey.clone();
            let app_handle = app.handle().clone();
            app.global_shortcut().on_shortcut(hk, move |app, _shortcut, event| {
                if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                    show_panel(app);
                    let _ = app_handle;
                }
            }).unwrap_or_else(|e| {
                eprintln!("hotkey registration failed (conflict?): {}", e);
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::search, commands::recent, commands::delete_item, commands::pin_item,
            commands::pause_recording, commands::get_config, commands::set_config,
            commands::add_exclude, commands::remove_exclude, commands::clear_all,
            commands::export_archive, commands::import_archive, commands::diagnostics,
            commands::item_text, commands::paste_item, commands::paste_stack, commands::copy_item,
        ])
        .run(tauri::generate_context!())
        .expect("error while running fastPaste");
}
