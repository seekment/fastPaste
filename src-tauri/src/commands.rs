//! Tauri IPC commands consumed by the WebView front-end.
use crate::config::Config;
use crate::models::*;
use crate::monitor::{MonitorCmd, MonitorHandle, MonitorEvent};
use crate::store::Store;
use parking_lot::Mutex;
use std::sync::Arc;
use serde::Serialize;
use std::sync::mpsc::channel;
use tauri::{Emitter, Manager, State};

pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub monitor: MonitorHandle,
    pub log: Mutex<Vec<String>>,
}

impl AppState {
    pub fn log(&self, msg: String) {
        let mut g = self.log.lock();
        g.push(format!("{} {}", now_ms(), msg));
        let n = g.len();
        if n > 500 { g.drain(..n - 500); }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardDto {
    pub id: u64,
    pub content_type: String,
    pub source_app: String,
    pub created_at: i64,
    pub pinned: bool,
    pub preview: String,
    pub text_length: usize,
    pub thumb: Option<String>, // base64 PNG data URL
}

fn to_dto(st: &Store, meta: &ClipMeta) -> CardDto {
    let thumb = meta.thumb_ref.as_ref().and_then(|_| {
        st.read_blob(meta.id).ok().flatten().map(|b| {
            format!("data:image/png;base64,{}", base64_encode(&b))
        })
    });
    CardDto {
        id: meta.id,
        content_type: format!("{:?}", meta.content_type).to_lowercase(),
        source_app: meta.source_app.clone(),
        created_at: meta.created_at,
        pinned: meta.pinned,
        preview: meta.preview.clone(),
        text_length: meta.text_length,
        thumb,
    }
}

fn base64_encode(data: &[u8]) -> String {
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TBL[(n >> 18) as usize & 63] as char);
        out.push(TBL[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TBL[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TBL[n as usize & 63] as char } else { '=' });
    }
    out
}

fn ctype_from(s: &str) -> Option<ContentType> {
    Some(match s {
        "text" => ContentType::Text,
        "rtf" => ContentType::Rtf,
        "html" => ContentType::Html,
        "link" => ContentType::Link,
        "image" => ContentType::Image,
        "files" => ContentType::Files,
        _ => return None,
    })
}

#[tauri::command]
pub fn search(state: State<'_, AppState>, query: String, filter: String, app: Option<String>, limit: usize) -> Vec<CardDto> {
    let st = state.store.lock();
    let ct = ctype_from(&filter);
    let hits = st.index.search(&query, limit, ct, app.as_deref());
    let mut out: Vec<CardDto> = hits.iter().map(|h| to_dto(&st, st.index.get(h.id).unwrap())).collect();
    out.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.created_at.cmp(&a.created_at)));
    out
}

#[tauri::command]
pub fn recent(state: State<'_, AppState>, filter: String, limit: usize) -> Vec<CardDto> {
    let st = state.store.lock();
    let ids = st.index.recent(limit, ctype_from(&filter), None);
    ids.iter().filter_map(|&id| st.index.get(id)).map(|m| to_dto(&st, m)).collect()
}

#[tauri::command]
pub fn delete_item(state: State<'_, AppState>, id: u64) -> Result<bool, String> {
    state.store.lock().delete(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn pin_item(state: State<'_, AppState>, id: u64, pinned: bool) -> Result<bool, String> {
    state.store.lock().set_pin(id, pinned).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn pause_recording(state: State<'_, AppState>, pause: bool) -> Result<(), String> {
    state.store.lock().config.pause_recording = pause;
    Ok(())
}

#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> Config {
    state.store.lock().config.clone()
}

#[tauri::command]
pub fn set_config(state: State<'_, AppState>, cfg: Config) -> Result<(), String> {
    let mut st = state.store.lock();
    st.config = cfg.clone();
    let p = st.dir.join("config.json");
    st.config.save(&p).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_exclude(state: State<'_, AppState>, app: String) -> Result<(), String> {
    let mut st = state.store.lock();
    if !st.config.excluded_apps.contains(&app) {
        st.config.excluded_apps.push(app);
    }
    let p = st.dir.join("config.json");
    st.config.save(&p).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove_exclude(state: State<'_, AppState>, app: String) -> Result<(), String> {
    let mut st = state.store.lock();
    st.config.excluded_apps.retain(|a| a != &app);
    let p = st.dir.join("config.json");
    st.config.save(&p).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_all(state: State<'_, AppState>) -> Result<(), String> {
    state.store.lock().clear_all().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn export_archive(state: State<'_, AppState>, path: String) -> Result<(), String> {
    state.store.lock().export_archive(std::path::Path::new(&path)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn import_archive(state: State<'_, AppState>, path: String) -> Result<usize, String> {
    state.store.lock().import_archive(std::path::Path::new(&path)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn diagnostics(state: State<'_, AppState>) -> Vec<String> {
    state.log.lock().clone()
}

#[tauri::command]
pub fn item_text(state: State<'_, AppState>, id: u64) -> Result<String, String> {
    let st = state.store.lock();
    let payload = st.read_payload(id).map_err(|e| e.to_string())?;
    Ok(payload.text.unwrap_or_default())
}

/// FR-K03/K05 + FR-A01: paste item (optionally as plain text) to the previously
/// focused app. Clipboard write goes through the serial monitor thread (FR-S06);
/// panel hiding and keystroke injection happen afterwards.
#[tauri::command]
pub fn paste_item(app: tauri::AppHandle, state: State<'_, AppState>, id: u64, plain: bool, keep_open: bool) -> Result<(), String> {
    let (text, html) = {
        let st = state.store.lock();
        let payload = st.read_payload(id).map_err(|e| e.to_string())?;
        let meta = st.index.get(id).ok_or("no such item")?.clone();
        match meta.content_type {
            ContentType::Image => {
                // image write-back requires bitmap clipboard support; degrade to
                // copy-only with an explicit hint (FR-K08 / FR-A05 提示原因)
                state.log(format!("paste degraded to copy-only for image item {}", id));
                return Ok(());
            }
            ContentType::Files => {
                // copy file list back as text paths
                (payload.files.map(|f| f.join("\n")), None)
            }
            ContentType::Html if !plain => {
                let t = payload.text.clone().or_else(|| payload.html.as_ref().map(|h| html_to_text(h)));
                let h = payload.html.clone();
                (t, h)
            }
            _ => {
                let t = payload.text.clone()
                    .or_else(|| payload.html.as_ref().map(|h| html_to_text(h)))
                    .or(payload.rtf);
                (t, None)
            }
        }
    };
    let text = text.unwrap_or_default();
    let (tx, rx) = channel();
    state.monitor.cmd.send(MonitorCmd::Write { text: Some(text), html, reply: tx })
        .map_err(|e| e.to_string())?;
    if !rx.recv().map_err(|e| e.to_string())? {
        return Err("clipboard write failed".into());
    }

    if !keep_open {
        if let Some(w) = app.get_webview_window("panel") {
            let _ = w.hide();
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(80)); // let target app regain focus

    if !crate::paste::can_inject() {
        state.log("paste degraded to copy-only (no accessibility / sandbox)".into());
        return Ok(()); // FR-K08: content is on clipboard; user pastes manually
    }
    crate::paste::paste_shortcut()
}

/// FR-A02 Paste Stack: paste selected ids in order with separator.
#[tauri::command]
pub fn paste_stack(app: tauri::AppHandle, state: State<'_, AppState>, ids: Vec<u64>, separator: String, keep_open: bool) -> Result<(), String> {
    let mut parts = Vec::new();
    {
        let st = state.store.lock();
        for &id in &ids {
            let payload = st.read_payload(id).map_err(|e| e.to_string())?;
            let meta = st.index.get(id).ok_or("no such item")?.clone();
            let part = match meta.content_type {
                ContentType::Files => payload.files.map(|f| f.join("\n")).unwrap_or_default(),
                _ => payload.text.clone()
                    .or_else(|| payload.html.as_ref().map(|h| html_to_text(h)))
                    .unwrap_or_default(),
            };
            parts.push(part);
        }
    }
    let joined = crate::paste::join_stack(&parts, &separator);
    let (tx, rx) = channel();
    state.monitor.cmd.send(MonitorCmd::Write { text: Some(joined), html: None, reply: tx })
        .map_err(|e| e.to_string())?;
    if !rx.recv().map_err(|e| e.to_string())? {
        return Err("clipboard write failed".into());
    }
    if !keep_open {
        if let Some(w) = app.get_webview_window("panel") { let _ = w.hide(); }
    }
    std::thread::sleep(std::time::Duration::from_millis(80));
    if crate::paste::can_inject() {
        crate::paste::paste_shortcut()
    } else {
        Ok(())
    }
}

#[tauri::command]
pub fn copy_item(state: State<'_, AppState>, id: u64, plain: bool) -> Result<(), String> {
    let (text, html) = {
        let st = state.store.lock();
        let payload = st.read_payload(id).map_err(|e| e.to_string())?;
        let meta = st.index.get(id).ok_or("no such item")?.clone();
        match meta.content_type {
            ContentType::Html if !plain => (payload.text, payload.html),
            _ => (payload.text.or_else(|| payload.html.as_ref().map(|h| html_to_text(h))), None),
        }
    };
    let (tx, rx) = channel();
    state.monitor.cmd.send(MonitorCmd::Write { text: text.or_else(|| Some(String::new())), html, reply: tx })
        .map_err(|e| e.to_string())?;
    if rx.recv().map_err(|e| e.to_string())? { Ok(()) } else { Err("clipboard write failed".into()) }
}

/// Drains monitor events (captured items / suspension) and forwards to panel.
pub fn forward_events(app: tauri::AppHandle, events: std::sync::mpsc::Receiver<MonitorEvent>) {
    std::thread::spawn(move || {
        while let Ok(ev) = events.recv() {
            match ev {
                MonitorEvent::Captured(m) => {
                    let _ = app.emit_to("panel", "clip", m);
                }
                MonitorEvent::Suspended(b) => {
                    let _ = app.emit_to("panel", "monitor-suspended", b);
                }
            }
        }
    });
}

// ---------------- shared panel / hotkey helpers (used by main.rs too) ----------------

/// Show the panel honoring the configured layout (FR-K06).
pub fn show_panel_cmd(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("panel") {
        let layout = if let Some(st) = app.try_state::<AppState>() {
            st.store.lock().config.panel_layout.clone()
        } else {
            "overlay".to_string()
        };
        if layout == "compact" {
            // small dock-style card window at the right edge
            let _ = w.set_size(tauri::LogicalSize { width: 400.0, height: 560.0 });
            if let Ok(Some(m)) = app.primary_monitor() {
                let pos = tauri::PhysicalPosition {
                    x: (m.size().width as i32) - 420,
                    y: (m.size().height as i32 - 600) / 2,
                };
                let _ = w.set_position(pos);
            }
        } else {
            let _ = w.set_size(tauri::LogicalSize { width: 980.0, height: 620.0 });
            let _ = w.center();
        }
        let _ = w.show();
        let _ = w.set_focus();
        let _ = app.emit_to("panel", "panel-shown", ());
    }
}

/// FR-K01: (re)register the global hotkey. Used at startup and from settings.
pub fn register_hotkey(app: &tauri::AppHandle, hotkey_str: &str) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let hk: tauri_plugin_global_shortcut::Shortcut = hotkey_str.parse()
        .map_err(|_| format!("invalid hotkey: {}", hotkey_str))?;
    app.global_shortcut().on_shortcut(hk, hotkey_handler)
        .map_err(|e| format!("hotkey registration failed: {}", e))
}

fn hotkey_handler(app: &tauri::AppHandle, _s: &tauri_plugin_global_shortcut::Shortcut,
                  event: tauri_plugin_global_shortcut::ShortcutEvent) {
    use tauri_plugin_global_shortcut::ShortcutState;
    if event.state == ShortcutState::Pressed {
        show_panel_cmd(app);
    }
}

/// FR-B02: toggle launch-on-login (FR-B02, requires tauri-plugin-autostart).
#[tauri::command]
pub fn set_auto_launch(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let al = app.autolaunch();
    if enabled { al.enable() } else { al.disable() }
        .map_err(|e| format!("auto-launch failed: {}", e))
}

/// FR-B03: change the global hotkey at runtime (no restart needed).
#[tauri::command]
pub fn update_hotkey(app: tauri::AppHandle, state: State<'_, AppState>, hotkey: String) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let old_str = state.store.lock().config.hotkey.clone();
    let old: tauri_plugin_global_shortcut::Shortcut = old_str.parse()
        .map_err(|_| format!("old hotkey invalid: {}", old_str))?;
    let _ = app.global_shortcut().unregister(old);
    register_hotkey(&app, &hotkey)?;
    let mut st = state.store.lock();
    st.config.hotkey = hotkey;
    let p = st.dir.join("config.json");
    st.config.save(&p).map_err(|e| e.to_string())?;
    Ok(())
}
