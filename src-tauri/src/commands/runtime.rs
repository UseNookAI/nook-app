//! The runtime (`ui/src/api/runtime.ts`): the hub's first-start steps (HubScreen.kt), the GPU load
//! the brand mark spins with, and Settings › Runtime's status, unload, pin and GPU snapshot.
//! Thin wrappers over `nook_core::runtime::RuntimeManager`.

use std::collections::BTreeMap;

use nook_core::runtime::{Snapshot, Status};
use serde::Serialize;
use tauri::State;

use super::{err, CmdResult};
use crate::AppState;

/// What the hub needs before its first-start card goes away.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInit {
    /// Models are installed but the text engine is not: the hub installs it in the background.
    needs_engine: bool,
    /// "NVIDIA CUDA 12", "Vulkan", "CPU".
    backend_label: String,
}

#[tauri::command]
pub async fn runtime_init(state: State<'_, AppState>) -> CmdResult<RuntimeInit> {
    let runtime = &state.0.runtime;
    Ok(RuntimeInit {
        needs_engine: runtime.needs_engine().await,
        backend_label: runtime.backend().await.label().to_string(),
    })
}

/// Reads the installed models and the catalog again (ModelDownloadService.refreshSync).
#[tauri::command]
pub async fn runtime_refresh_models(state: State<'_, AppState>) -> CmdResult<()> {
    state.0.downloads.refresh_sync().await;
    Ok(())
}

/// Installs the text engine for the backend (the hub's background install): true when installed,
/// false when it stopped; the failure's message otherwise, which the hub shows after "Runtime
/// install failed: " as `RuntimeManager::install_missing_engine` words it.
#[tauri::command]
pub async fn runtime_ensure_engine(state: State<'_, AppState>) -> CmdResult<bool> {
    let cancel = state.0.stopping();
    state
        .0
        .runtime
        .ensure_engine_installed(None, &cancel)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn runtime_gpu_load(state: State<'_, AppState>) -> CmdResult<f64> {
    Ok(state.0.runtime.gpu_load().await)
}

#[tauri::command]
pub async fn runtime_status(state: State<'_, AppState>) -> CmdResult<Status> {
    Ok(state.0.runtime.status().await)
}

#[tauri::command]
pub fn runtime_downloads(state: State<'_, AppState>) -> BTreeMap<String, f64> {
    state.0.runtime.downloads()
}

#[tauri::command]
pub async fn runtime_unload(state: State<'_, AppState>, model_id: String) -> CmdResult<()> {
    state.0.runtime.unload(&model_id).await;
    Ok(())
}

#[tauri::command]
pub fn runtime_pin(state: State<'_, AppState>, model_id: String, pin: bool) {
    state.0.runtime.pin(&model_id, pin);
}

/// Loads a model now (RuntimeManager.load); nothing when it is already resident. No screen of the
/// original had a Load button (models load on first use, as a Code turn's does); this is for QA
/// and the live checks.
#[tauri::command]
pub async fn runtime_load(state: State<'_, AppState>, model_id: String) -> CmdResult<()> {
    state
        .0
        .runtime
        .load(&model_id)
        .await
        .map(|_| ())
        .map_err(err)
}

#[tauri::command]
pub async fn gpu_snapshot(state: State<'_, AppState>) -> CmdResult<Snapshot> {
    Ok(state.0.runtime.inventory().snapshot().await)
}
