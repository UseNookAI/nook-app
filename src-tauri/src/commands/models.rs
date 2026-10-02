//! Models (`ui/src/api/models.ts`): the curated catalog, the installed models and the library's
//! downloads (`models_*`, over `ModelDownloadService`), the Hugging Face browser (`hub_*`), which
//! installed model does each kind of work (`workers_*`, `runtime\workers.json`) and the worker's
//! web access (`web_access_*`).
//!
//! The UI names a library model by its id where `ModelDownloadService` takes an `AiModelDto`: the
//! id is looked up in the service's library (`downloads::all_available_models`, as its last
//! refresh keeps it).

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use nook_core::code::CodeChanged;
use nook_core::events::{self, topic};
use nook_core::runtime::downloads::{self, NOOK_REGISTRY};
use nook_core::runtime::{
    AiModelDto, CatalogModel, LocalModel, Repo, RuntimeManager, Variant, CODE_WORKER,
};
use nook_core::usage::Tool;
use nook_core::Nook;
use serde::Serialize;
use tauri::State;

use super::{blocking, err, msg, CmdResult};
use crate::AppState;

// ------------------------------------------------------------------ catalog and installed models

/// The curated catalog with the default model per task.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    models: Vec<CatalogModel>,
    default_chat_model: Option<String>,
    default_worker_model: Option<String>,
    default_speech_model: Option<String>,
    default_image_model: Option<String>,
    default_video_model: Option<String>,
}

/// The fields of the download service's state the Models page reads (`DownloadsState`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadsView {
    is_loading: bool,
    downloading_models: Vec<String>,
    paused_models: Vec<String>,
    stopping_models: Vec<String>,
    deleting_models: Vec<String>,
    downloading_progress: BTreeMap<String, f64>,
    text_downloading_count: u32,
}

#[tauri::command]
pub fn models_catalog(state: State<'_, AppState>) -> Catalog {
    let catalog = state.0.runtime.catalog();
    let own = |id: Option<&str>| id.map(str::to_string);
    Catalog {
        models: catalog.all().to_vec(),
        default_chat_model: own(catalog.default_chat_model()),
        default_worker_model: own(catalog.default_worker_model()),
        default_speech_model: own(catalog.default_speech_model()),
        default_image_model: own(catalog.default_image_model()),
        default_video_model: own(catalog.default_video_model()),
    }
}

/// The models on disk, shared ones included, without those still downloading, paused or being
/// stopped (as `ModelDownloadService.refresh_sync` counts them).
#[tauri::command]
pub async fn models_installed(state: State<'_, AppState>) -> CmdResult<Vec<LocalModel>> {
    let s = state.0.downloads.state();
    let in_flight: HashSet<String> = s
        .downloading_models
        .into_iter()
        .chain(s.paused_models)
        .chain(s.stopping_models)
        .collect();
    let registry = state.0.runtime.registry().clone();
    blocking(move || {
        Ok(registry
            .list()
            .into_iter()
            .filter(|m| !in_flight.contains(&m.id))
            .collect())
    })
    .await
}

#[tauri::command]
pub fn models_downloads(state: State<'_, AppState>) -> DownloadsView {
    let s = state.0.downloads.state();
    DownloadsView {
        is_loading: s.is_loading,
        downloading_models: s.downloading_models,
        paused_models: s.paused_models,
        stopping_models: s.stopping_models,
        deleting_models: s.deleting_models,
        downloading_progress: s.downloading_progress,
        text_downloading_count: s.text_downloading_count,
    }
}

/// The library entry for an id: from the service's last refresh, else read afresh.
pub(crate) async fn library_model(nook: &Arc<Nook>, id: &str) -> Option<AiModelDto> {
    let known = nook.downloads.state().available_models;
    if let Some(m) = known.into_iter().find(|m| m.model == id) {
        return Some(m);
    }
    let runtime: Arc<RuntimeManager> = nook.runtime.clone();
    let id = id.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        downloads::all_available_models(&runtime)
            .into_iter()
            .find(|m| m.model == id)
    })
    .await
    .ok()
    .flatten()
}

/// A library entry for a key the library no longer lists (a model removed by hand mid-download):
/// enough to pause, resume, stop or delete it.
pub(crate) fn bare_model(id: &str) -> AiModelDto {
    AiModelDto {
        model: id.to_string(),
        model_registry: Some(NOOK_REGISTRY.to_string()),
        ..Default::default()
    }
}

/// Starts downloading a catalog model, its engine first when missing.
#[tauri::command]
pub async fn models_download(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let model = library_model(&state.0, &id)
        .await
        .ok_or_else(|| format!("Unknown catalog model {id}"))?;
    state.0.downloads.launch_download(&model);
    state.0.usage.used(Tool::ModelDownload);
    Ok(())
}

#[tauri::command]
pub fn models_pause(state: State<'_, AppState>, id: String) {
    state.0.downloads.pause_download(&id);
}

#[tauri::command]
pub async fn models_resume(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let model = library_model(&state.0, &id)
        .await
        .unwrap_or_else(|| bare_model(&id));
    state.0.downloads.resume_download(&model);
    Ok(())
}

/// Stops a download and deletes what it wrote.
#[tauri::command]
pub async fn models_cancel(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let model = library_model(&state.0, &id)
        .await
        .unwrap_or_else(|| bare_model(&id));
    state.0.downloads.stop_download(&model);
    Ok(())
}

/// Deletes an installed model, unloading it first; the service does it in the background and the
/// "downloads" topic tells the page. A shared model (the installed Nook's) is refused at once
/// with `ModelRegistry::delete`'s message, since the service would only log it.
#[tauri::command]
pub async fn models_delete(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let registry = state.0.runtime.registry().clone();
    let key = id.clone();
    blocking(move || {
        if registry.find(&key).is_some_and(|m| m.shared) {
            // refuses without touching anything
            registry.delete(&key)?;
        }
        Ok(())
    })
    .await?;
    let model = library_model(&state.0, &id)
        .await
        .unwrap_or_else(|| bare_model(&id));
    state.0.downloads.delete_model(&model);
    Ok(())
}

// ------------------------------------------------------------------ Hugging Face

#[tauri::command]
pub async fn hub_search(
    state: State<'_, AppState>,
    query: String,
    limit: Option<u32>,
) -> CmdResult<Vec<Repo>> {
    state
        .0
        .runtime
        .hub()
        .search(&query, limit.unwrap_or(30))
        .await
        .map_err(msg)
}

#[tauri::command]
pub async fn hub_variants(state: State<'_, AppState>, repo_id: String) -> CmdResult<Vec<Variant>> {
    state.0.runtime.hub().variants(&repo_id).await.map_err(msg)
}

/// The keys of the variants already in the models folder (this home's or the shared one).
#[tauri::command]
pub async fn hub_installed(
    state: State<'_, AppState>,
    repo_id: String,
    variants: Vec<Variant>,
) -> CmdResult<Vec<String>> {
    let hub = state.0.runtime.hub().clone();
    blocking(move || {
        Ok(variants
            .iter()
            .filter(|v| hub.is_installed(&repo_id, v))
            .map(|v| v.key.clone())
            .collect())
    })
    .await
}

/// Downloads a variant in the background, the text engine first when missing; false when that
/// variant is already downloading.
#[tauri::command]
pub async fn hub_download(
    state: State<'_, AppState>,
    repo: Repo,
    variant: Variant,
) -> CmdResult<bool> {
    let started = state.0.runtime.download_hub_async(repo, variant);
    if started {
        state.0.usage.used(Tool::ModelDownload);
    }
    Ok(started)
}

/// Stops a hub download; the partial file stays so the next attempt resumes.
#[tauri::command]
pub fn hub_cancel(state: State<'_, AppState>, repo_id: String, variant_key: String) {
    state.0.runtime.cancel_hub_download(&repo_id, &variant_key);
}

// ------------------------------------------------------------------ workers

/// Task to chosen model id, plus the `thinking`, `slots` and `ctx` switches.
#[tauri::command]
pub async fn workers_preferences(
    state: State<'_, AppState>,
) -> CmdResult<BTreeMap<String, String>> {
    let registry = state.0.runtime.registry().clone();
    blocking(move || Ok(registry.worker_preferences())).await
}

/// The model serving each task now, after preferences and defaults; tasks with none are left out.
#[tauri::command]
pub async fn workers_current(state: State<'_, AppState>) -> CmdResult<BTreeMap<String, String>> {
    let registry = state.0.runtime.registry().clone();
    let code = state.0.code.clone();
    blocking(move || {
        let mut current = BTreeMap::new();
        // The Code service's worker: its own menu changes it too.
        if let Some(m) = code.worker() {
            current.insert(CODE_WORKER.to_string(), m.id);
        }
        for task in ["chat", "embed", "speech", "image", "video"] {
            if let Some(m) = registry.worker_for(task) {
                current.insert(task.to_string(), m.id);
            }
        }
        Ok(current)
    })
    .await
}

/// Remembers (or with null clears) the model for a task in workers.json. A new Code worker is
/// told to the Code pages too, as the Code menu's own choice is.
#[tauri::command]
pub async fn workers_set(
    state: State<'_, AppState>,
    task: String,
    model_id: Option<String>,
) -> CmdResult<()> {
    let registry = state.0.runtime.registry().clone();
    let code = task == CODE_WORKER;
    blocking(move || registry.set_worker_preference(&task, model_id.as_deref())).await?;
    if code {
        events::emit(topic::CODE, CodeChanged { session_id: None });
    }
    Ok(())
}

// ------------------------------------------------------------------ web access

#[tauri::command]
pub fn web_access_enabled(state: State<'_, AppState>) -> bool {
    state.0.web.enabled()
}

#[tauri::command]
pub fn web_access_set(state: State<'_, AppState>, enabled: bool) -> CmdResult<()> {
    state.0.web.set_enabled(enabled).map_err(err)
}
