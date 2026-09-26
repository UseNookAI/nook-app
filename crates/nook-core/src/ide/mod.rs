//! The Code page (editor) backend: the folder tree, file reads and writes, new/rename/delete, the
//! folder's git branch, and `<home>\code\ide.json`. Ports the non-UI parts of `ui/component/ide`:
//! `IdeSupport.kt` (listing, binary and line-ending rules), `IdeWorkspace.kt` (reading, saving and
//! changing files, the branch) and `IdePrefs.kt` (what the page keeps between starts).
//!
//! The editor itself (CodeMirror), the explorer's open folders and the open tabs live in the UI
//! (`ui/src/screens/ide`); everything that touches the disk is here, and the Tauri commands in
//! `src-tauri/src/commands/ide.rs` are thin wrappers over it.

mod files;
mod prefs;

pub use files::*;
pub use prefs::*;
