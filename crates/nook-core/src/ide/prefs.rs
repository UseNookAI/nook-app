//! What the Code page keeps between starts (`IdePrefs.kt`): the folder, the open files, the panels
//! and their widths, and which Nook session belongs to which folder. One JSON file,
//! `<home>\code\ide.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::write_atomic;
use crate::Home;

/// What a side panel may be dragged to, in px (dp in the original).
pub const MIN_PANEL: i32 = 180;
pub const MAX_PANEL: i32 = 720;

const DEFAULT_EXPLORER_WIDTH: i32 = 240;
const DEFAULT_ASSISTANT_WIDTH: i32 = 400;

/// `<home>\code\ide.json`.
pub fn prefs_file(home: &Home) -> PathBuf {
    home.code_dir().join("ide.json")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IdePrefs {
    pub folder: Option<String>,
    pub open: Vec<String>,
    pub active: Option<String>,
    pub explorer_width: i32,
    pub assistant_width: i32,
    pub explorer_open: bool,
    pub assistant_open: bool,
    /// The Nook session of each folder, by the folder's path.
    pub sessions: BTreeMap<String, String>,
}

impl Default for IdePrefs {
    fn default() -> Self {
        IdePrefs {
            folder: None,
            open: Vec::new(),
            active: None,
            explorer_width: DEFAULT_EXPLORER_WIDTH,
            assistant_width: DEFAULT_ASSISTANT_WIDTH,
            explorer_open: true,
            assistant_open: true,
            sessions: BTreeMap::new(),
        }
    }
}

/// One save at a time: two at once raced on the same temporary file (2026-09-25).
static SAVING: Mutex<()> = Mutex::new(());

impl IdePrefs {
    /// The file's contents, or the defaults when it is missing or cannot be read. Each field is
    /// read on its own, so one of the wrong type falls back to its default; widths come back
    /// within [`MIN_PANEL`]..=[`MAX_PANEL`].
    pub fn load(file: &Path) -> IdePrefs {
        let Ok(text) = std::fs::read_to_string(file) else {
            return IdePrefs::default();
        };
        match serde_json::from_str::<Value>(crate::settings::strip_bom_str(&text)) {
            Ok(Value::Object(n)) => {
                let text_of = |key: &str| n.get(key).and_then(Value::as_str).map(str::to_string);
                IdePrefs {
                    folder: text_of("folder"),
                    open: n
                        .get("open")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    active: text_of("active"),
                    explorer_width: as_int(n.get("explorerWidth"), DEFAULT_EXPLORER_WIDTH)
                        .clamp(MIN_PANEL, MAX_PANEL),
                    assistant_width: as_int(n.get("assistantWidth"), DEFAULT_ASSISTANT_WIDTH)
                        .clamp(MIN_PANEL, MAX_PANEL),
                    explorer_open: as_bool(n.get("explorerOpen"), true),
                    assistant_open: as_bool(n.get("assistantOpen"), true),
                    sessions: n
                        .get("sessions")
                        .and_then(Value::as_object)
                        .map(|s| {
                            s.iter()
                                .filter_map(|(k, v)| {
                                    v.as_str().map(|id| (k.clone(), id.to_string()))
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                }
            }
            _ => IdePrefs::default(),
        }
    }

    /// Writes through a temporary file, so a crash mid-write leaves the previous version.
    pub fn save(&self, file: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        let _one_at_a_time = SAVING.lock();
        write_atomic(file, &json)
    }

    /// The prefs with a folder that is no longer there left out, as the page opens them.
    pub fn without_missing_folder(mut self) -> IdePrefs {
        if self
            .folder
            .as_deref()
            .is_some_and(|f| !Path::new(f).is_dir())
        {
            self.folder = None;
        }
        self
    }
}

/// Jackson's `asInt(default)`: a number (truncated), a numeric string, else the default.
fn as_int(v: Option<&Value>, default: i32) -> i32 {
    let n = match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok(),
        _ => None,
    };
    n.map(|n| n.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
        .unwrap_or(default)
}

/// Jackson's `asBoolean(default)`: a boolean, "true"/"false", a number (non-zero is true), else the default.
fn as_bool(v: Option<&Value>, default: bool) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => match s.trim() {
            "true" => true,
            "false" => false,
            _ => default,
        },
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_page_remembers_its_state_in_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("code").join("ide.json");
        assert_eq!(IdePrefs::load(&file), IdePrefs::default());
        let prefs = IdePrefs {
            folder: Some("F:\\work\\app".into()),
            open: vec!["F:\\work\\app\\a.kt".into(), "F:\\work\\app\\b.kt".into()],
            active: Some("F:\\work\\app\\b.kt".into()),
            explorer_width: 300,
            assistant_width: 420,
            explorer_open: false,
            assistant_open: true,
            sessions: BTreeMap::from([("F:\\work\\app".to_string(), "ab12cd34".to_string())]),
        };
        prefs.save(&file).unwrap();
        assert_eq!(IdePrefs::load(&file), prefs);
        // A width out of range comes back within it; a broken file gives the defaults.
        std::fs::write(&file, r#"{"explorerWidth": 5, "assistantWidth": 9999}"#).unwrap();
        assert_eq!(IdePrefs::load(&file).explorer_width, MIN_PANEL);
        assert_eq!(IdePrefs::load(&file).assistant_width, MAX_PANEL);
        std::fs::write(&file, "{not json").unwrap();
        assert_eq!(IdePrefs::load(&file), IdePrefs::default());
    }

    #[test]
    fn fields_of_the_wrong_type_fall_back_one_by_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ide.json");
        std::fs::write(
            &file,
            r#"{"folder": 7, "open": ["a", 3, "b"], "explorerWidth": "300", "explorerOpen": "false",
                "assistantOpen": 0, "sessions": {"x": "s1", "y": 2}}"#,
        )
        .unwrap();
        let p = IdePrefs::load(&file);
        assert_eq!(p.folder, None);
        assert_eq!(p.open, vec!["a", "b"]);
        assert_eq!(p.explorer_width, 300);
        assert_eq!(p.assistant_width, DEFAULT_ASSISTANT_WIDTH);
        assert!(!p.explorer_open);
        assert!(!p.assistant_open);
        assert_eq!(
            p.sessions,
            BTreeMap::from([("x".to_string(), "s1".to_string())])
        );
    }

    #[test]
    fn the_file_uses_the_original_field_names() {
        let json = serde_json::to_value(IdePrefs::default()).unwrap();
        let keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for key in [
            "folder",
            "open",
            "active",
            "explorerWidth",
            "assistantWidth",
            "explorerOpen",
            "assistantOpen",
            "sessions",
        ] {
            assert!(keys.contains(&key), "{key} missing from {keys:?}");
        }
        // The UI may send only some fields; the rest are the defaults.
        let partial: IdePrefs = serde_json::from_str(r#"{"folder": "C:\\x"}"#).unwrap();
        assert_eq!(partial.explorer_width, DEFAULT_EXPLORER_WIDTH);
    }

    #[test]
    fn saves_at_once_never_break_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("code").join("ide.json");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let file = file.clone();
                std::thread::spawn(move || {
                    for j in 0..10 {
                        let prefs = IdePrefs {
                            explorer_width: 200 + i * 10 + j,
                            ..IdePrefs::default()
                        };
                        prefs.save(&file).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let loaded = IdePrefs::load(&file);
        assert!((200..=279).contains(&loaded.explorer_width));
        let left: Vec<_> = std::fs::read_dir(file.parent().unwrap()).unwrap().collect();
        assert_eq!(left.len(), 1, "temporary files left behind");
    }

    #[test]
    fn a_folder_that_is_gone_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let here = IdePrefs {
            folder: Some(dir.path().display().to_string()),
            ..IdePrefs::default()
        };
        assert_eq!(here.clone().without_missing_folder(), here);
        let gone = IdePrefs {
            folder: Some(dir.path().join("gone").display().to_string()),
            ..IdePrefs::default()
        };
        assert_eq!(gone.without_missing_folder().folder, None);
    }

    #[test]
    fn the_file_lives_in_the_code_folder() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        assert_eq!(prefs_file(&home), dir.path().join("code").join("ide.json"));
    }
}
