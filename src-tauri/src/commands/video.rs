//! The Video page (`ui/src/api/video.ts`): the clip queue (`nook_core::video::VideoStudio`), the
//! setup its page reads (`VideoSetup`), the video model's download over the model downloads
//! (`ModelDownloadService`, as `VideoScreen.kt` drove it), and opening clips and their folder in
//! Windows (the original's `Desktop.open` and `explorer.exe /select,`).

use nook_core::video::{Clip, DownloadState, VideoSetup};
use tauri::State;

use super::models::{bare_model, library_model};
use super::{blocking, err, CmdResult};
use crate::AppState;

/// Every clip, newest first.
#[tauri::command]
pub fn video_clips(state: State<'_, AppState>) -> Vec<Clip> {
    state.0.video.clips()
}

/// Queues a clip (the queue's worker starts with the first one, on the async runtime);
/// `modelId` null is the preferred installed video model.
#[tauri::command]
pub async fn video_submit(
    state: State<'_, AppState>,
    prompt: String,
    model_id: Option<String>,
) -> CmdResult<Clip> {
    state
        .0
        .video
        .submit(&prompt, model_id.as_deref())
        .map_err(err)
}

/// Stops a queued or running clip; a finished one is left as it is.
#[tauri::command]
pub fn video_cancel(state: State<'_, AppState>, id: String) {
    state.0.video.cancel(&id);
}

/// Removes a finished clip and its files; false for an unknown or unfinished clip.
#[tauri::command]
pub async fn video_delete(state: State<'_, AppState>, id: String) -> CmdResult<bool> {
    let studio = state.0.video.clone();
    blocking(move || Ok(studio.delete(&id))).await
}

/// What the page needs before a clip can be asked for, for the model a clip would use. Reads the
/// models folder, so it runs on the blocking pool.
#[tauri::command]
pub async fn video_setup(
    state: State<'_, AppState>,
    model_id: Option<String>,
) -> CmdResult<VideoSetup> {
    // The page read the library once it was up (`refreshIfEmpty`), for its download card.
    state.0.downloads.refresh_if_empty();
    let runtime = state.0.runtime.clone();
    let studio = state.0.video.clone();
    tauri::async_runtime::spawn_blocking(move || {
        tauri::async_runtime::block_on(VideoSetup::read(&runtime, &studio, model_id.as_deref()))
    })
    .await
    .map_err(err)
}

/// Where the video model's download is.
#[tauri::command]
pub fn video_download_state(state: State<'_, AppState>, model_id: String) -> DownloadState {
    DownloadState::of(&state.0.downloads.state(), &model_id)
}

/// The model's files, and the sd engine when it is missing.
#[tauri::command]
pub async fn video_download(state: State<'_, AppState>, model_id: String) -> CmdResult<()> {
    let model = library_model(&state.0, &model_id)
        .await
        .ok_or_else(|| format!("Unknown catalog model {model_id}"))?;
    state.0.downloads.launch_download(&model);
    Ok(())
}

#[tauri::command]
pub fn video_download_pause(state: State<'_, AppState>, model_id: String) {
    state.0.downloads.pause_download(&model_id);
}

#[tauri::command]
pub async fn video_download_resume(state: State<'_, AppState>, model_id: String) -> CmdResult<()> {
    let model = library_model(&state.0, &model_id)
        .await
        .unwrap_or_else(|| bare_model(&model_id));
    state.0.downloads.resume_download(&model);
    Ok(())
}

/// Creates the videos folder when needed and opens it in Explorer.
#[tauri::command]
pub async fn video_open_folder(state: State<'_, AppState>) -> CmdResult<()> {
    let folder = state.0.video.folder().to_path_buf();
    blocking(move || {
        std::fs::create_dir_all(&folder)?;
        tauri_plugin_opener::open_path(&folder, None::<&str>)?;
        Ok(())
    })
    .await
}

/// The file of a finished clip.
fn clip_file(state: &State<'_, AppState>, id: &str) -> CmdResult<std::path::PathBuf> {
    state
        .0
        .video
        .clip(id)
        .and_then(|c| c.file)
        .ok_or_else(|| "That clip has no video yet.".to_string())
}

/// Opens a finished clip in the default video player.
#[tauri::command]
pub async fn video_open(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let file = clip_file(&state, &id)?;
    blocking(move || Ok(tauri_plugin_opener::open_path(&file, None::<&str>)?)).await
}

/// Shows a finished clip selected in Explorer.
#[tauri::command]
pub async fn video_reveal(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let file = clip_file(&state, &id)?;
    blocking(move || Ok(tauri_plugin_opener::reveal_item_in_dir(&file)?)).await
}
