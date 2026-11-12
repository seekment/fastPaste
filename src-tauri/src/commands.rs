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
    pub bound_key: Option<String>,
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
        bound_key: meta.bound_key.clone(),
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
    do_paste(&app, &state, id, plain, keep_open)
}

/// Internal paste implementation - shared by the paste command AND the
/// PasteMate-style bound-key global shortcuts (paste without opening the panel).
pub fn do_paste(app: &tauri::AppHandle, state: &AppState, id: u64, plain: bool, keep_open: bool) -> Result<(), String> {
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

    // PasteMate-style dock: a locked bottom strip never auto-hides.
    let docked = {
        let st = state.store.lock();
        st.config.panel_layout != "overlay" && st.config.dock_locked
    };
    if !keep_open && !docked {
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

/// Fit a size (fraction of the primary monitor) in physical pixels.
fn fit(mw: u32, mh: u32, frac_w: f64, frac_h: f64, max_w: f64, max_h: f64) -> (f64, f64) {
    let w = ((mw as f64) * frac_w).min(max_w).max(640.0);
    let h = ((mh as f64) * frac_h).min(max_h).max(360.0);
    (w, h)
}

/// Show the panel honoring the configured layout (FR-K06). Window geometry
/// adapts to the actual screen size (physical pixels, DPI-aware).
pub fn show_panel_cmd(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("panel") {
        let layout = if let Some(st) = app.try_state::<AppState>() {
            st.store.lock().config.panel_layout.clone()
        } else {
            "overlay".to_string()
        };
        let mon = app.primary_monitor().ok().flatten();
        let (mw, mh) = mon.as_ref()
            .map(|m| (m.size().width, m.size().height))
            .unwrap_or((1920, 1080));
        if layout == "strip" || layout == "compact" {
            // PasteMate-style bottom edge card strip (FR-K06 alt layout)
            let (pw, _ph) = fit(mw, mh, 0.82, 0.0, 1280.0, 240.0);
            let _ = w.set_size(tauri::PhysicalSize { width: pw as u32, height: 240 });
            let taskbar = 60; // leave room for the taskbar
            let _ = w.set_position(tauri::PhysicalPosition {
                x: ((mw as i32) - pw as i32) / 2,
                y: (mh as i32) - 240 - taskbar,
            });
        } else {
            // centered overlay card wall (Paste-style, FR-K06)
            let (pw, ph) = fit(mw, mh, 0.72, 0.72, 1180.0, 760.0);
            let _ = w.set_size(tauri::PhysicalSize { width: pw as u32, height: ph as u32 });
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

/// PasteMate-style "merge copies into one card": combine several text entries
/// into a single new history card, joined with newlines.
#[tauri::command]
pub fn merge_items(state: State<'_, AppState>, ids: Vec<u64>) -> Result<u64, String> {
    let mut parts: Vec<String> = Vec::new();
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
            if !part.is_empty() { parts.push(part); }
        }
    }
    if parts.is_empty() { return Err("nothing to merge".into()); }
    let clip = IncomingClip::text(&parts.join("\n"), "fastPaste");
    let mut st = state.store.lock();
    match st.record(clip).map_err(|e| e.to_string())? {
        crate::store::RecordOutcome::New(id) | crate::store::RecordOutcome::Duplicate(id) => Ok(id),
        _ => Err("merge blocked by privacy rules".into()),
    }
}

// ---------------- PasteMate-style features ----------------

/// Key binding: press the bound key anywhere -> this card pastes directly
/// (no panel). key=None unbinds. Registered as a GLOBAL shortcut, so users
/// should bind keys they don't need while typing (digits, F-keys...).
#[tauri::command]
pub fn set_bound_key(app: tauri::AppHandle, state: State<'_, AppState>, id: u64, key: Option<String>) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    // unregister previous binding for this card
    let old = state.store.lock().index.get(id).and_then(|m| m.bound_key.clone());
    if let Some(k) = old {
        if let Ok(sc) = k.parse::<tauri_plugin_global_shortcut::Shortcut>() {
            let _ = app.global_shortcut().unregister(sc);
        }
    }
    if let Some(k) = &key {
        let sc: tauri_plugin_global_shortcut::Shortcut = k.parse()
            .map_err(|_| format!("无法绑定的按键：{}（试试 1-9 或 F1-F12）", k))?;
        app.global_shortcut().on_shortcut(sc, move |app, _s, event| {
            if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                if let Some(st) = app.try_state::<AppState>() {
                    let _ = do_paste(app, &st, id, false, false);
                }
            }
        }).map_err(|e| format!("绑定失败（可能被其他程序占用）: {}", e))?;
    }
    state.store.lock().set_bound_key(id, key).map_err(|e| e.to_string())?;
    Ok(())
}

/// Re-register all bound keys at startup (bindings survive restarts).
pub fn register_bound_keys(app: &tauri::AppHandle) {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let items: Vec<(u64, String)> = {
        let st = app.state::<AppState>();
        let store = st.store.lock();
        store.index.snapshot().items.values()
            .filter_map(|m| m.bound_key.clone().map(|k| (m.id, k)))
            .collect()
    };
    for (id, key) in items {
        if let Ok(sc) = key.parse::<tauri_plugin_global_shortcut::Shortcut>() {
            let _ = app.global_shortcut().on_shortcut(sc, move |app, _s, event| {
                if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                    if let Some(st) = app.try_state::<AppState>() {
                        let _ = do_paste(app, &st, id, false, false);
                    }
                }
            });
        }
    }
}

/// Edit a card's text content in place (PasteMate-style).
#[tauri::command]
pub fn update_item_text(state: State<'_, AppState>, id: u64, text: String) -> Result<(), String> {
    state.store.lock().edit_text(id, &text).map_err(|e| e.to_string())?;
    Ok(())
}

/// Multi-delete (PasteMate-style batch ops).
#[tauri::command]
pub fn delete_items(state: State<'_, AppState>, ids: Vec<u64>) -> Result<usize, String> {
    let mut n = 0;
    for id in ids {
        if state.store.lock().delete(id).map_err(|e| e.to_string())? { n += 1; }
    }
    Ok(n)
}

/// Insert an empty card (memo).
#[tauri::command]
pub fn create_note(state: State<'_, AppState>) -> Result<u64, String> {
    match state.store.lock().create_note().map_err(|e| e.to_string())? {
        crate::store::RecordOutcome::New(id) | crate::store::RecordOutcome::Duplicate(id) => Ok(id),
        _ => Err("blocked by privacy rules".into()),
    }
}

/// Persist drag-to-order arrangement (strip layout).
#[tauri::command]
pub fn set_card_order(state: State<'_, AppState>, ids: Vec<u64>) -> Result<(), String> {
    let mut st = state.store.lock();
    st.config.card_order = ids;
    let p = st.dir.join("config.json");
    st.config.save(&p).map_err(|e| e.to_string())
}

/// PasteMate-style click-to-preview: expand the bottom strip to make room
/// for a preview pane above the card row. Esc / collapse shrinks it back.
#[tauri::command]
pub fn set_panel_expanded(app: tauri::AppHandle, expanded: bool) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("panel") {
        let layout = if let Some(st) = app.try_state::<AppState>() {
            st.store.lock().config.panel_layout.clone()
        } else { "overlay".into() };
        if layout == "strip" || layout == "compact" {
            let mon = app.primary_monitor().ok().flatten();
            let (mw, mh) = mon.as_ref().map(|m| (m.size().width, m.size().height)).unwrap_or((1920, 1080));
            let h = if expanded { 460 } else { 240 };
            let pw = ((mw as f64) * 0.82).min(1280.0) as u32;
            let _ = w.set_size(tauri::PhysicalSize { width: pw, height: h });
            let _ = w.set_position(tauri::PhysicalPosition {
                x: ((mw as i32) - pw as i32) / 2,
                y: (mh as i32) - (h as i32) - 60,
            });
        }
    }
    Ok(())
}
