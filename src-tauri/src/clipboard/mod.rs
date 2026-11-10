//! Clipboard capture layer. Platform choice (FR-S01 / FR-S08):
//! - Windows: AddClipboardFormatListener (WM_CLIPBOARDUPDATE) - no polling, no clipboard chain.
//! - macOS:   NSPasteboard.changeCount polling, 500 ms (1000 ms in low-power mode).
#[cfg(target_os = "windows")]
pub mod win;
#[cfg(target_os = "macos")]
pub mod mac;

/// What was on the clipboard - exactly ONE primary format per FR-S02 priority:
/// Files > Image > HTML > RTF > Text.
#[derive(Debug, Clone)]
pub enum RawClip {
    Text(String),
    Html(String),
    Rtf(String),
    Image { bytes: Vec<u8>, bmp_dib: bool },
    Files(Vec<String>),
}

impl RawClip {
    pub fn classify(&self) -> (crate::models::ContentType, String) {
        use crate::models::ContentType::*;
        match self {
            RawClip::Text(t) => {
                let s = t.trim_start();
                if s.starts_with("http://") || s.starts_with("https://") { (Link, t.clone()) } else { (Text, t.clone()) }
            }
            RawClip::Html(h) => (Html, h.clone()),
            RawClip::Rtf(r) => (Rtf, r.clone()),
            RawClip::Image { .. } => (Image, String::new()),
            RawClip::Files(fs) => (Files, fs.join("\n")),
        }
    }
}

/// Spawn the OS-specific change notifier; signals `tx` on every change.
pub fn spawn_listener(tx: std::sync::mpsc::Sender<()>) -> std::thread::JoinHandle<()> {
    #[cfg(target_os = "windows")]
    { win::spawn_listener(tx) }
    #[cfg(target_os = "macos")]
    { mac::spawn_listener(tx) }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    { std::thread::spawn(move || { let _ = tx; }) }
}

/// Read exactly one primary format from the clipboard (called ONLY from the
/// dedicated serial clipboard thread - FR-S06). Returns None when clipboard
/// is locked / transient / unreadable (FR-S03, FR-S04).
pub fn read_primary(app_override: Option<&str>) -> Option<(RawClip, String)> {
    #[cfg(target_os = "windows")]
    { win::read_primary(app_override) }
    #[cfg(target_os = "macos")]
    { mac::read_primary(app_override) }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    { let _ = app_override; None }
}

/// Write back to the clipboard: only the target content itself, no private
/// marker formats (FR-S05). Called only from the serial clipboard thread.
pub fn write_clipboard(text: Option<&str>, html: Option<&str>) -> bool {
    #[cfg(target_os = "windows")]
    { win::write_clipboard(text, html) }
    #[cfg(target_os = "macos")]
    { mac::write_clipboard(text, html) }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    { let _ = (text, html); false }
}
