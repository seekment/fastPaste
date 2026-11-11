//! FR-K03/K04/K08 + 3.4 tech constraints: paste execution.
//! Order: (1) write desired content to clipboard via the serial monitor thread,
//! (2) hide panel, (3) inject Ctrl+V / Cmd+V. Degrades to copy-only when
//! injection is impossible (sandbox / missing Accessibility permission).
use enigo::{Enigo, Keyboard, Settings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteMode {
    Normal,
    PlainText,
    CopyOnly, // FR-K08 / no accessibility permission
}

pub struct Paster {
    enigo: Option<Enigo>, // lazy: Enigo::new can fail on headless/wayland-less systems
}

impl Paster {
    pub fn new() -> Self {
        Self { enigo: None }
    }

    fn enigo(&mut self) -> Result<&mut Enigo, String> {
        if self.enigo.is_none() {
            self.enigo = Some(Enigo::new(&Settings::default()).map_err(|e| e.to_string())?);
        }
        Ok(self.enigo.as_mut().unwrap())
    }

    pub fn paste_shortcut(&mut self) -> Result<(), String> {
        use enigo::Key;
        let enigo = self.enigo()?;
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
