//! The per-user Nook home and the layout under it. Ports `AppHome.kt` and `runtime/RuntimePaths.java`.
//!
//! This app has its own identity beside the installed (Kotlin) Nook: its home is
//! `%LOCALAPPDATA%\Nook-rs` (`~/.nook-rs` elsewhere), overridable with `NOOK_RS_HOME`. It never
//! writes to the installed Nook's home; it may read that home's downloaded models (see
//! [`Home::shared_models_dir`]), and imports its sessions, worker files and engines once
//! ([`crate::migrate`]).
//!
//! ```text
//! <home>\data      settings.json and other user data
//! <home>\logs      nook.log
//! <home>\models    downloaded models
//! <home>\runtime   engines (bin\<backend>), downloads, engine logs
//! <home>\images    generated images
//! <home>\videos    generated clips with a JSON sidecar each
//! <home>\code      Code sessions and the editor's ide.json
//! <home>\tmp       scratch copies, voice recordings
//! <home>\gateway.json   the gateway's port and per-start token
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Environment variable that moves the home (tests, sandboxes).
pub const HOME_ENV: &str = "NOOK_RS_HOME";
/// Environment variable naming a read-only models folder to reuse; empty turns reuse off.
pub const SHARED_MODELS_ENV: &str = "NOOK_RS_SHARED_MODELS";
/// Folder name under %LOCALAPPDATA%.
pub const HOME_DIR_NAME: &str = "Nook-rs";

#[derive(Clone, Debug)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    /// Resolves the home from `NOOK_RS_HOME`, else `%LOCALAPPDATA%\Nook-rs`, else `~/.nook-rs`,
    /// and creates `data` and `logs`.
    pub fn resolve() -> Result<Home> {
        let root = match std::env::var(HOME_ENV) {
            Ok(v) if !v.trim().is_empty() => PathBuf::from(v.trim()),
            _ => match std::env::var("LOCALAPPDATA") {
                Ok(v) if !v.trim().is_empty() => PathBuf::from(v).join(HOME_DIR_NAME),
                _ => user_home().join(".nook-rs"),
            },
        };
        let home = Home::at(root);
        home.create_basics()?;
        Ok(home)
    }

    /// A home at an explicit path (tests). Creates nothing.
    pub fn at(root: impl Into<PathBuf>) -> Home {
        let root = root.into();
        let root = std::path::absolute(&root).unwrap_or(root);
        Home { root }
    }

    fn create_basics(&self) -> Result<()> {
        for dir in [self.data_dir(), self.logs_dir()] {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("Could not create {}", dir.display()))?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn log_file(&self) -> PathBuf {
        self.logs_dir().join("nook.log")
    }
    pub fn settings_file(&self) -> PathBuf {
        self.data_dir().join("settings.json")
    }
    pub fn runtime_dir(&self) -> PathBuf {
        self.root.join("runtime")
    }
    /// Engine binaries for one backend id (`cuda`, `vulkan`, `cpu`).
    pub fn bin_dir(&self, backend_id: &str) -> PathBuf {
        self.runtime_dir().join("bin").join(backend_id)
    }
    pub fn downloads_dir(&self) -> PathBuf {
        self.runtime_dir().join("downloads")
    }
    pub fn engine_logs_dir(&self) -> PathBuf {
        self.runtime_dir().join("logs")
    }
    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }
    /// Generated images, one PNG per request.
    pub fn images_dir(&self) -> PathBuf {
        self.root.join("images")
    }
    /// Generated videos: one clip per request with a JSON sidecar that remembers the prompt.
    pub fn videos_dir(&self) -> PathBuf {
        self.root.join("videos")
    }
    /// The flows' runs: one folder per run with what it wrote and a `run.json`.
    pub fn flows_dir(&self) -> PathBuf {
        self.root.join("flows")
    }
    /// The voice models the flows speak with, apart from the chat and speech models so they are
    /// not listed with them.
    pub fn voices_dir(&self) -> PathBuf {
        self.root.join("voices")
    }
    /// Code sessions (`code\sessions`) and the editor's `code\ide.json`.
    pub fn code_dir(&self) -> PathBuf {
        self.root.join("code")
    }
    /// Scratch space: the worker's scratch copies (`tmp\code`), voice recordings.
    pub fn temp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }
    pub fn gateway_file(&self) -> PathBuf {
        self.root.join("gateway.json")
    }

    /// The installed Nook's models folder, reused read-only so gigabytes are not downloaded twice.
    /// `NOOK_RS_SHARED_MODELS` names another folder, or turns this off when empty. None when the
    /// folder does not exist or is this home's own models folder.
    pub fn shared_models_dir(&self) -> Option<PathBuf> {
        let dir = match std::env::var(SHARED_MODELS_ENV) {
            Ok(v) if v.trim().is_empty() => return None,
            Ok(v) => PathBuf::from(v.trim()),
            Err(_) => PathBuf::from(std::env::var("LOCALAPPDATA").ok()?)
                .join("Nook")
                .join("models"),
        };
        if !dir.is_dir() || same_path(&dir, &self.models_dir()) {
            return None;
        }
        Some(dir)
    }

    /// Creates the runtime layout (downloads, engine logs, models).
    pub fn ensure_layout(&self) -> Result<()> {
        for dir in [
            self.downloads_dir(),
            self.engine_logs_dir(),
            self.models_dir(),
            self.temp_dir(),
            self.code_dir(),
        ] {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("Could not create {}", dir.display()))?;
        }
        Ok(())
    }
}

fn user_home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_hangs_off_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        assert_eq!(
            home.bin_dir("cuda"),
            dir.path().join("runtime").join("bin").join("cuda")
        );
        assert_eq!(home.gateway_file(), dir.path().join("gateway.json"));
        home.ensure_layout().unwrap();
        assert!(home.models_dir().is_dir());
    }
}
