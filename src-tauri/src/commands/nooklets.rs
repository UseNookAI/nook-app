//! The Nooklets' finder (`ui/src/api/nooklets.ts`): the Nooklet for a request typed in a
//! sentence (`nook_core::nooklets::Finder`), and the finder's one download (its progress on the
//! "nooklets" topic).

use nook_core::nooklets::{FinderSetup, Found};
use tauri::State;

use super::CmdResult;
use crate::AppState;

#[tauri::command]
pub fn nooklets_setup(state: State<'_, AppState>) -> FinderSetup {
    state.0.nooklets.setup()
}

/// Every Nooklet for `request`, the best first.
#[tauri::command]
pub async fn nooklets_find(state: State<'_, AppState>, request: String) -> CmdResult<Found> {
    Ok(state.0.nooklets.find(&request).await)
}

#[tauri::command]
pub async fn nooklets_install(state: State<'_, AppState>) -> CmdResult<()> {
    state.0.nooklets.start_install()
}

#[tauri::command]
pub fn nooklets_cancel_install(state: State<'_, AppState>) {
    state.0.nooklets.cancel_install();
}

#[tauri::command]
pub fn nooklets_clear_install_error(state: State<'_, AppState>) {
    state.0.nooklets.clear_install_error();
}
