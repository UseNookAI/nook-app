//! App-level commands: build info, settings, erase everything, quit, the licence and notices.

use std::collections::BTreeMap;

use nook_core::build_info::BuildInfo;
use serde::Serialize;
use tauri::State;

use super::{err, CmdResult};
use crate::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    build: BuildInfo,
    label: String,
    home: String,
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    let build = BuildInfo::current();
    AppInfo {
        label: build.label(),
        build,
        home: state.0.home.root().display().to_string(),
    }
}

#[tauri::command]
pub fn settings_all(state: State<'_, AppState>) -> BTreeMap<String, String> {
    state.0.settings.all()
}

#[tauri::command]
pub fn settings_set(state: State<'_, AppState>, name: String, value: String) -> CmdResult<()> {
    state.0.settings.set(&name, value).map_err(err)?;
    nook_core::events::emit(
        nook_core::events::topic::SETTINGS,
        serde_json::json!({ "name": name }),
    );
    Ok(())
}

/// Settings › General › Erase everything: settings back to defaults and the log emptied. The
/// caller quits afterwards.
#[tauri::command]
pub fn app_erase_everything(state: State<'_, AppState>) -> CmdResult<()> {
    let _ = std::fs::remove_file(state.0.home.log_file());
    state.0.settings.reset().map_err(err)
}

#[tauri::command]
pub fn app_quit(app: tauri::AppHandle) {
    app.exit(0);
}

/// What quitting now would cut short or lose, in words ("a PDF has changes that are not saved
/// yet", "a flow is running"): the window asks before it closes while there is any.
#[tauri::command]
pub fn app_quit_check(state: State<'_, AppState>) -> Vec<String> {
    state.0.updater.busy_all()
}

/// The software licence for Settings › About (resources/eula.html), a complete HTML document.
#[tauri::command]
pub fn app_eula() -> &'static str {
    nook_core::resources::EULA_HTML
}

/// The third-party notices for Settings › About (resources/third-party-notices.html).
#[tauri::command]
pub fn app_notices() -> &'static str {
    nook_core::resources::THIRD_PARTY_NOTICES_HTML
}
