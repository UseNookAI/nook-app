//! The document converter Nooklet (`ui/src/api/convert.ts`): what files can become
//! (`nook_core::convert::ConvertService::offer`), the engines' one download (its progress on the
//! "convert" topic), the conversions (each job's progress on the same topic), and opening a
//! result in Windows.

use std::path::PathBuf;

use nook_core::convert::routes::Engine;
use nook_core::convert::{Job, Offer};
use nook_core::flow::Install;
use tauri::State;

use super::{blocking, err, msg, CmdResult};
use crate::AppState;

/// What `paths` can become, with what each target still needs.
#[tauri::command]
pub async fn convert_offer(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<Offer> {
    let convert = state.0.convert.clone();
    blocking(move || {
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        Ok(convert.offer(&paths))
    })
    .await
}

/// Starts the download of `engines` (those not in yet).
#[tauri::command]
pub async fn convert_install(state: State<'_, AppState>, engines: Vec<Engine>) -> CmdResult<()> {
    state.0.convert.start_install(&engines)
}

#[tauri::command]
pub fn convert_install_state(state: State<'_, AppState>) -> Option<Install> {
    state.0.convert.install_state()
}

#[tauri::command]
pub fn convert_cancel_install(state: State<'_, AppState>) {
    state.0.convert.cancel_install();
}

#[tauri::command]
pub fn convert_clear_install_error(state: State<'_, AppState>) {
    state.0.convert.clear_install_error();
}

/// Converts `paths` into `to`: beside each file, or into `folder`; pictures into one PDF when
/// `combine`.
#[tauri::command]
pub async fn convert_start(
    state: State<'_, AppState>,
    paths: Vec<String>,
    to: String,
    combine: bool,
    folder: Option<String>,
) -> CmdResult<Job> {
    state
        .0
        .convert
        .start(
            paths.into_iter().map(PathBuf::from).collect(),
            &to,
            combine,
            folder.map(PathBuf::from),
        )
        .map_err(msg)
}

/// The jobs so far, newest first.
#[tauri::command]
pub fn convert_jobs(state: State<'_, AppState>) -> Vec<Job> {
    state.0.convert.jobs()
}

#[tauri::command]
pub fn convert_cancel(state: State<'_, AppState>, id: String) {
    state.0.convert.cancel(&id);
}

fn result(state: &State<'_, AppState>, path: &str) -> CmdResult<PathBuf> {
    let p = PathBuf::from(path);
    if !state.0.convert.is_output(&p) {
        return Err(err("That is not a converted file."));
    }
    Ok(p)
}

/// Opens a result in the program Windows opens it with (a folder of pages in Explorer).
#[tauri::command]
pub async fn convert_open(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    let p = result(&state, &path)?;
    blocking(move || Ok(tauri_plugin_opener::open_path(&p, None::<&str>)?)).await
}

/// Shows a result selected in Explorer.
#[tauri::command]
pub async fn convert_reveal(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    let p = result(&state, &path)?;
    blocking(move || Ok(tauri_plugin_opener::reveal_item_in_dir(&p)?)).await
}
