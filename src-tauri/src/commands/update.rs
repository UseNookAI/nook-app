//! Self-update (`ui/src/api/update.ts`): thin wrappers over `nook_core::update::Updater`. Every
//! change of state goes out as a full `UpdateStatus` on the "update" topic, so the commands that
//! start something return at once.

use nook_core::events::{self, topic};
use nook_core::settings::UPDATE_CHANNEL;
use nook_core::update::UpdateStatus;
use tauri::State;

use super::{err, CmdResult};
use crate::AppState;

#[tauri::command]
pub fn update_status(state: State<'_, AppState>) -> UpdateStatus {
    state.0.updater.status()
}

/// Starts a check in the background (nothing when no update base is configured).
#[tauri::command]
pub async fn update_check(state: State<'_, AppState>) -> CmdResult<()> {
    state.0.updater.check_for_updates();
    Ok(())
}

/// Downloads, verifies and runs the installer on offer; the updater's quit hook then exits.
#[tauri::command]
pub async fn update_start(state: State<'_, AppState>) -> CmdResult<()> {
    state.0.updater.start_download_and_install(false);
    Ok(())
}

/// Stops the installer download; its partial file is deleted.
#[tauri::command]
pub fn update_cancel(state: State<'_, AppState>) {
    state.0.updater.cancel_download();
}

/// "Later": hides the offer until a new one arrives.
#[tauri::command]
pub fn update_snooze(state: State<'_, AppState>) {
    state.0.updater.snooze();
}

/// Saves the channel, drops the other channel's offer and checks again.
#[tauri::command]
pub async fn update_set_channel(state: State<'_, AppState>, channel: String) -> CmdResult<()> {
    state.0.updater.set_channel(&channel).map_err(err)?;
    events::emit(
        topic::SETTINGS,
        serde_json::json!({ "name": UPDATE_CHANNEL }),
    );
    Ok(())
}
