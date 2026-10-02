//! Code sessions (`ui/src/api/code.ts`): thin wrappers over `nook_core::code::CodeService`, as
//! the table in `nook_core::code`'s docs maps them, plus the voice-input install card's download
//! (`code_speech_download`, `code_speech_install`), which reads and drives the model downloads
//! for the catalog's default speech model as the original's `CodeModels.SpeechInstall` did.
//!
//! What reads the models folder (the snapshot, the worker menu, the next context, the speech
//! check) runs on the blocking pool; turns run as tokio tasks the service starts itself.

use nook_core::code::{CodeService, CodeSession, CodeSnapshot, EditorContext, NextContext};
use nook_core::code::{RepositoryState, SpeechModel};
use nook_core::usage::Tool;
use serde::Serialize;
use tauri::State;

use super::models::library_model;
use super::{blocking, err, CmdResult};
use crate::AppState;

fn service(state: &State<'_, AppState>) -> CodeService {
    state.0.code.clone()
}

/// Everything Code mode shows; re-read on every "code" event (and "downloads" ones, since a
/// finished download changes the worker menu).
#[tauri::command]
pub async fn code_snapshot(state: State<'_, AppState>) -> CmdResult<CodeSnapshot> {
    let code = service(&state);
    blocking(move || Ok(code.snapshot())).await
}

/// Starts a session on `folder` and its first turn; the new session, with the turn under way.
#[tauri::command]
pub async fn code_start(
    state: State<'_, AppState>,
    folder: String,
    text: String,
    verify: Option<String>,
    context: Option<EditorContext>,
) -> CmdResult<CodeSession> {
    let started = service(&state)
        .start(&folder, &text, verify.as_deref(), context.as_ref())
        .await
        .map_err(err);
    state.0.usage.outcome(Tool::CodeSession, &started);
    started
}

/// Asks for the next change in a session; returns once the turn is under way.
#[tauri::command]
pub async fn code_send(
    state: State<'_, AppState>,
    id: String,
    text: String,
    verify: Option<String>,
    context: Option<EditorContext>,
) -> CmdResult<()> {
    let sent = service(&state)
        .send(&id, &text, verify.as_deref(), context.as_ref())
        .await
        .map_err(err);
    state.0.usage.outcome(Tool::CodeTurn, &sent);
    sent
}

#[tauri::command]
pub fn code_stop(state: State<'_, AppState>, id: String) {
    state.0.code.stop(&id);
}

#[tauri::command]
pub async fn code_apply(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let applied = service(&state).apply(&id).await.map_err(err);
    state.0.usage.outcome(Tool::CodeApply, &applied);
    applied
}

#[tauri::command]
pub async fn code_discard(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    service(&state).discard(&id).await.map_err(err)
}

#[tauri::command]
pub async fn code_undo(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    service(&state).undo(&id).await.map_err(err)
}

#[tauri::command]
pub async fn code_delete(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    service(&state).delete(&id).await;
    Ok(())
}

#[tauri::command]
pub fn code_rename(state: State<'_, AppState>, id: String, title: String) {
    state.0.code.rename(&id, &title);
}

/// The code one run wrote, or null.
#[tauri::command]
pub async fn code_run_diff(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
) -> CmdResult<Option<String>> {
    Ok(service(&state).run_diff(&session_id, &run_id).await)
}

/// What the next request of a session starts with; null without a session or a worker.
#[tauri::command]
pub async fn code_next_context(
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<Option<NextContext>> {
    let code = service(&state);
    blocking(move || Ok(code.next_context(&id))).await
}

#[tauri::command]
pub async fn code_repository_state(
    state: State<'_, AppState>,
    folder: String,
) -> CmdResult<RepositoryState> {
    service(&state).repository_state(&folder).await.map_err(err)
}

#[tauri::command]
pub fn code_recent_repositories(state: State<'_, AppState>) -> Vec<String> {
    state.0.code.recent_repositories()
}

/// Makes a model the worker for the next request (Settings › Models › Workers shows it too).
#[tauri::command]
pub async fn code_set_worker(state: State<'_, AppState>, model_id: String) -> CmdResult<()> {
    let code = service(&state);
    blocking(move || code.set_worker(&model_id)).await
}

/// Null when voice input can run, else what is missing (the speech model or its engine).
#[tauri::command]
pub async fn code_speech_problem(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    let code = service(&state);
    blocking(move || Ok(code.speech_problem())).await
}

/// The speech model Nook installs for voice input (the catalog's default), or null.
#[tauri::command]
pub fn code_speech_model(state: State<'_, AppState>) -> Option<SpeechModel> {
    state.0.code.speech_model()
}

/// The speech model's download, for the install card.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechDownload {
    /// The library offers the model.
    available: bool,
    downloading: bool,
    /// 0..1, null before the first byte.
    progress: Option<f64>,
}

#[tauri::command]
pub async fn code_speech_download(state: State<'_, AppState>) -> CmdResult<SpeechDownload> {
    let Some(model) = state.0.code.speech_model() else {
        return Ok(SpeechDownload {
            available: false,
            downloading: false,
            progress: None,
        });
    };
    // The card read the library once it was up (`refreshIfEmpty`).
    state.0.downloads.refresh_if_empty();
    let library = state.0.downloads.state();
    Ok(SpeechDownload {
        available: library.available_models.iter().any(|m| m.model == model.id),
        downloading: library.downloading_models.contains(&model.id),
        progress: library.downloading_progress.get(&model.id).copied(),
    })
}

/// Starts downloading the speech model and, when missing, its engine.
#[tauri::command]
pub async fn code_speech_install(state: State<'_, AppState>) -> CmdResult<()> {
    let model = state
        .0
        .code
        .speech_model()
        .ok_or_else(|| "Nook has no speech model to install.".to_string())?;
    let dto = library_model(&state.0, &model.id)
        .await
        .ok_or_else(|| format!("Unknown catalog model {}", model.id))?;
    state.0.downloads.launch_download(&dto);
    Ok(())
}
