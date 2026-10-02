//! Settings › General › Usage statistics (`ui/src/api/usage.ts`): whether the daily report goes,
//! the next report exactly as it would be sent, and the one-time notice
//! (`nook_core::usage::Usage`).

use nook_core::usage::Overview;
use tauri::State;

use super::{msg, CmdResult};
use crate::AppState;

/// Whether reports are on, whether this build sends any, the last one's time and the next one.
#[tauri::command]
pub async fn usage_overview(state: State<'_, AppState>) -> CmdResult<Overview> {
    Ok(state.0.usage.overview().await)
}

/// Turns the reports on or off; off drops the counts that were waiting.
#[tauri::command]
pub fn usage_set(state: State<'_, AppState>, enabled: bool) -> CmdResult<()> {
    state.0.usage.set_enabled(enabled).map_err(msg)?;
    nook_core::events::emit(
        nook_core::events::topic::SETTINGS,
        serde_json::json!({ "name": nook_core::settings::SHARE_USAGE }),
    );
    Ok(())
}

/// The notice about the reports has been on screen.
#[tauri::command]
pub fn usage_notice_seen(state: State<'_, AppState>) -> CmdResult<()> {
    state.0.usage.notice_seen().map_err(msg)
}
