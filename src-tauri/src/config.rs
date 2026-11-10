//! FR-B03 / FR-P01 / FR-P04 / FR-K01: persisted settings (config.json).
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub hotkey: String,            // e.g. "Control+Shift+V" / "Command+Shift+V"
    pub retention_days: Option<u32>,
    pub max_disk_bytes: Option<u64>,
    pub excluded_apps: Vec<String>,
    pub pause_recording: bool,
    pub panel_layout: String,      // "overlay" | "edge"
    pub paste_keep_open: bool,
    pub encrypt: bool,             // FR-P06 (AES-256-GCM key lives in OS keychain; placeholder flag)
    pub paste_aware: bool,         // FR-S09 感知粘贴 mode (macOS default true)
    pub auto_launch: bool,         // FR-B02
    pub privacy_notice_ack: bool,  // FR-B05 / FR-S09
}

impl Default for Config {
    fn default() -> Self {
        #[cfg(target_os = "macos")]
        let hotkey = "Command+Shift+V".to_string();
        #[cfg(not(target_os = "macos"))]
        let hotkey = "Control+Shift+V".to_string();
        Self {
            hotkey,
            retention_days: None,
            max_disk_bytes: Some(2 * 1024 * 1024 * 1024), // 2 GB default cap
            excluded_apps: vec![],
            pause_recording: false,
            panel_layout: "overlay".into(),
            paste_keep_open: false,
            encrypt: false,
            paste_aware: cfg!(target_os = "macos"),
            auto_launch: false,
            privacy_notice_ack: false,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Atomic write: tmp + sync + rename (NFR-R02).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            use std::io::Write;
            write!(f, "{}", serde_json::to_string_pretty(self).unwrap())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }

    /// FR-P01/NFR-S04: exclusion matched on app name or executable path, case-insensitive.
    pub fn is_excluded(&self, app: &str) -> bool {
        let a = app.to_lowercase();
        self.excluded_apps.iter().any(|e| a.contains(&e.to_lowercase()) || e.to_lowercase().contains(&a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusion_match_is_flexible() {
        let mut c = Config::default();
        c.excluded_apps.push("1Password".into());
        assert!(c.is_excluded("1Password 7"));
        assert!(c.is_excluded("/Applications/1Password.app/Contents/MacOS/1Password"));
        assert!(!c.is_excluded("Chrome"));
    }
    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir();
        let p = dir.join("fastpaste-cfg-test.json");
        let mut c = Config::default();
        c.hotkey = "Alt+V".into();
        c.save(&p).unwrap();
        let c2 = Config::load(&p);
        assert_eq!(c2.hotkey, "Alt+V");
        std::fs::remove_file(&p).ok();
    }
}
