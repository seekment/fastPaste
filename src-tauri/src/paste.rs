//! FR-K03/K04/K08 + 3.4 tech constraints: paste execution.
//! Order: (1) write desired content to clipboard via the serial monitor thread,
//! (2) hide panel, (3) inject Ctrl+V / Cmd+V. Degrades to copy-only when
//! injection is impossible (sandbox / missing Accessibility permission).
//!
//! NOTE: enigo's macOS Enigo holds a CGEventSource (NonNull) and is NOT Send,
//! so it must never live inside Tauri managed state. A fresh Enigo is created
//! per paste call instead (user-triggered, low frequency).
use enigo::{Enigo, Keyboard, Settings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteMode {
    Normal,
    PlainText,
    CopyOnly, // FR-K08 / no accessibility permission
}

/// Send the platform paste shortcut to the currently focused app.
pub fn paste_shortcut() -> Result<(), String> {
    use enigo::Key;
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    {
        enigo.key(Key::Meta, enigo::Direction::Press).map_err(|e| e.to_string())?;
        enigo.key(Key::Unicode('v'), enigo::Direction::Click).map_err(|e| e.to_string())?;
        enigo.key(Key::Meta, enigo::Direction::Release).map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        enigo.key(Key::Control, enigo::Direction::Press).map_err(|e| e.to_string())?;
        enigo.key(Key::Unicode('v'), enigo::Direction::Click).map_err(|e| e.to_string())?;
        enigo.key(Key::Control, enigo::Direction::Release).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// FR-K08 / 3.4: decide whether keystroke injection is allowed on this platform.
pub fn can_inject() -> bool {
    #[cfg(target_os = "macos")]
    { crate::clipboard::mac::accessibility_trusted() }
    #[cfg(not(target_os = "macos"))]
    { true } // admin-elevation caveat documented in settings UI (3.4)
}

/// Paste stack separator (FR-A02): entries joined with newline or user string.
pub fn join_stack(parts: &[String], separator: &str) -> String {
    parts.join(separator)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stack_joins_in_order() {
        assert_eq!(join_stack(&["a".into(), "b".into()], "\n"), "a\nb");
        assert_eq!(join_stack(&["1".into(), "2".into(), "3".into()], ", "), "1, 2, 3");
    }
}
