//! The app's key/value settings. Ports the H2 `global_property` table (`GlobalPropertyConfig.java`,
//! `NookAgentService.java`): the same names and defaults, kept as strings in
//! `<home>\data\settings.json` and written atomically.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use parking_lot::RwLock;

pub const IS_EULA_ACCEPTED: &str = "IS_EULA_ACCEPTED";
pub const IS_SETUP_COMPLETED: &str = "IS_SETUP_COMPLETED";
pub const SETUP_VERSION: &str = "SETUP_VERSION";
pub const USAGE_PREFERENCES: &str = "USAGE_PREFERENCES";
pub const TOKEN_HISTORY_LIMIT: &str = "TOKEN_HISTORY_LIMIT";
pub const MAX_PINNED_MESSAGES: &str = "MAX_PINNED_MESSAGES";
/// Light, Dark or System.
pub const APP_THEME: &str = "APP_THEME";
pub const IS_PRIVATE_MODE: &str = "IS_PRIVATE_MODE";
pub const IS_ADVANCED_MODE: &str = "IS_ADVANCED_MODE";
pub const LAST_ACTIVE_SCREEN: &str = "LAST_ACTIVE_SCREEN";
/// stable, or dev for every build of main.
pub const UPDATE_CHANNEL: &str = "UPDATE_CHANNEL";

/// Every property with its default, created when missing (`InitializeGlobalProperties`).
pub const DEFAULTS: &[(&str, &str)] = &[
    (IS_EULA_ACCEPTED, "false"),
    (IS_SETUP_COMPLETED, "false"),
    (SETUP_VERSION, "0"),
    (USAGE_PREFERENCES, ""),
    (TOKEN_HISTORY_LIMIT, "128000"),
    (MAX_PINNED_MESSAGES, "10"),
    (APP_THEME, "Light"),
    (IS_PRIVATE_MODE, "false"),
    (IS_ADVANCED_MODE, "false"),
    (LAST_ACTIVE_SCREEN, ""),
    (UPDATE_CHANNEL, "stable"),
];

pub struct Settings {
    path: PathBuf,
    values: RwLock<BTreeMap<String, String>>,
}

impl Settings {
    /// Loads the file (a missing or unreadable one starts empty) and fills in missing defaults.
    pub fn load(path: impl Into<PathBuf>) -> Result<Settings> {
        let path = path.into();
        let values: BTreeMap<String, String> = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(strip_bom(&bytes)).unwrap_or_else(|e| {
                tracing::warn!(
                    "settings file {} unreadable ({e}); starting from defaults",
                    path.display()
                );
                BTreeMap::new()
            }),
            Err(_) => BTreeMap::new(),
        };
        let settings = Settings {
            path,
            values: RwLock::new(values),
        };
        settings.init_defaults()?;
        Ok(settings)
    }

    fn init_defaults(&self) -> Result<()> {
        let mut changed = false;
        {
            let mut map = self.values.write();
            for (name, value) in DEFAULTS {
                if !map.contains_key(*name) {
                    map.insert((*name).to_string(), (*value).to_string());
                    changed = true;
                }
            }
        }
        if changed {
            self.save()?;
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<String> {
        self.values.read().get(name).cloned()
    }

    pub fn get_or(&self, name: &str, fallback: &str) -> String {
        self.get(name).unwrap_or_else(|| fallback.to_string())
    }

    pub fn get_bool(&self, name: &str) -> bool {
        self.get(name)
            .map(|v| v.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }

    pub fn get_i64(&self, name: &str) -> Option<i64> {
        self.get(name).and_then(|v| v.trim().parse().ok())
    }

    /// Sets and persists one value.
    pub fn set(&self, name: &str, value: impl Into<String>) -> Result<()> {
        self.values.write().insert(name.to_string(), value.into());
        self.save()
    }

    pub fn all(&self) -> BTreeMap<String, String> {
        self.values.read().clone()
    }

    /// Back to defaults (Erase everything).
    pub fn reset(&self) -> Result<()> {
        self.values.write().clear();
        self.init_defaults()?;
        self.save()
    }

    fn save(&self) -> Result<()> {
        let json = serde_json::to_vec_pretty(&*self.values.read())?;
        write_atomic(&self.path, &json)
    }
}

/// Drops a UTF-8 byte order mark. Files saved by PowerShell or an older Notepad start with one;
/// serde_json refuses it where the original's Jackson read past it, so every file a person may edit
/// by hand (settings.json, workers.json, web.json, ide.json, a repository's nook.json) goes
/// through this first.
pub fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)
}

/// [`strip_bom`] for text already read as UTF-8.
pub fn strip_bom_str(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Writes through a sibling temp file and a rename, so a crash never leaves half a file.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Could not create {}", parent.display()))?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes).with_context(|| format!("Could not write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("Could not replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_saved_with_a_byte_order_mark_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, b"\xEF\xBB\xBF{\"APP_THEME\": \"Dark\"}").unwrap();
        let s = Settings::load(&file).unwrap();
        assert_eq!(s.get(APP_THEME).as_deref(), Some("Dark"));
    }

    #[test]
    fn defaults_are_created_and_values_persist() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data").join("settings.json");
        let s = Settings::load(&file).unwrap();
        assert_eq!(s.get(APP_THEME).as_deref(), Some("Light"));
        assert!(!s.get_bool(IS_SETUP_COMPLETED));
        s.set(IS_SETUP_COMPLETED, "true").unwrap();
        let again = Settings::load(&file).unwrap();
        assert!(again.get_bool(IS_SETUP_COMPLETED));
        again.reset().unwrap();
        assert!(!again.get_bool(IS_SETUP_COMPLETED));
    }
}
