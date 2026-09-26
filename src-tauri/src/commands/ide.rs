//! The Code page (editor): its folder tree, the files it opens and saves, the explorer's changes,
//! and `<home>\code\ide.json`. Thin wrappers over `nook_core::ide`; the file work runs on the
//! blocking pool so a big folder or file never holds up the window.

use std::path::{Path, PathBuf};

use nook_core::ide::{self, FileNode, IdePrefs, LoadedFile};
use tauri::State;

use super::{blocking, CmdResult};
use crate::AppState;

/// What the page remembered last time; a folder that is gone is left out.
#[tauri::command]
pub async fn ide_load_prefs(state: State<'_, AppState>) -> CmdResult<IdePrefs> {
    let file = ide::prefs_file(&state.0.home);
    blocking(move || Ok(IdePrefs::load(&file).without_missing_folder())).await
}

/// Remembers the page's state. The page sends one save at a time; the core also serializes them.
#[tauri::command]
pub async fn ide_save_prefs(state: State<'_, AppState>, prefs: IdePrefs) -> CmdResult<()> {
    let file = ide::prefs_file(&state.0.home);
    blocking(move || prefs.save(&file)).await
}

/// The folder as the page keys it (absolute, normalized).
#[tauri::command]
pub fn ide_resolve_folder(path: String) -> String {
    ide::normalize_folder(Path::new(&path))
        .display()
        .to_string()
}

/// The branch checked out in the folder, or null outside git.
#[tauri::command]
pub async fn ide_branch(dir: String) -> CmdResult<Option<String>> {
    Ok(ide::read_branch(Path::new(&dir)).await)
}

/// A folder's entries: folders first, by name; `.git` left out; an unreadable folder is empty.
#[tauri::command]
pub async fn ide_list_dir(dir: String) -> CmdResult<Vec<FileNode>> {
    blocking(move || Ok(ide::list_entries(Path::new(&dir)))).await
}

/// A file as text for the editor; refused when binary or over 4 MB.
#[tauri::command]
pub async fn ide_read_file(path: String) -> CmdResult<LoadedFile> {
    blocking(move || ide::read_text(Path::new(&path))).await
}

/// Saves the editor's text with the file's line ending and charset; returns the new modification time.
#[tauri::command]
pub async fn ide_write_file(
    path: String,
    text: String,
    line_ending: String,
    charset: String,
) -> CmdResult<i64> {
    blocking(move || ide::write_text(Path::new(&path), &text, &line_ending, &charset)).await
}

/// The modification time of each path that is a file (null for the rest), to notice changes made elsewhere.
#[tauri::command]
pub async fn ide_file_times(paths: Vec<String>) -> CmdResult<Vec<Option<i64>>> {
    blocking(move || {
        Ok(ide::file_times(
            &paths.into_iter().map(PathBuf::from).collect::<Vec<_>>(),
        ))
    })
    .await
}

#[tauri::command]
pub async fn ide_create_file(dir: String, name: String) -> CmdResult<String> {
    blocking(move || {
        Ok(ide::create_file(Path::new(&dir), &name)?
            .display()
            .to_string())
    })
    .await
}

#[tauri::command]
pub async fn ide_create_folder(dir: String, name: String) -> CmdResult<String> {
    blocking(move || {
        Ok(ide::create_folder(Path::new(&dir), &name)?
            .display()
            .to_string())
    })
    .await
}

#[tauri::command]
pub async fn ide_rename(path: String, name: String) -> CmdResult<FileNode> {
    blocking(move || ide::rename(Path::new(&path), &name)).await
}

/// Deletes a file, or a folder with everything in it, for good.
#[tauri::command]
pub async fn ide_delete(path: String) -> CmdResult<()> {
    blocking(move || ide::delete(Path::new(&path))).await
}
