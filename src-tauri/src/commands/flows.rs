//! The Flows page (`ui/src/api/flows.ts`): the runs (`nook_core::flow::FlowService`), what a run
//! would do and still needs (`Plan`), the one download that brings it in, the microphone for a
//! spoken run (the voice prompt's recorder), and opening a run's files in Windows.
//!
//! A plan and a download open the chosen file's headers, and the recorder blocks briefly, so they
//! run on the blocking pool (whose threads are inside the async runtime, where a run's queue and a
//! download start).

use std::path::PathBuf;

use nook_core::flow::voices::{Offered, Voices};
use nook_core::flow::{Install, Order, Peek, Plan, PlanInput, Run};
use tauri::State;

use super::{blocking, err, CmdResult};
use crate::AppState;

fn plan_input(input: Option<String>, microphone: bool) -> PlanInput {
    match input.filter(|p| !p.trim().is_empty()) {
        _ if microphone => PlanInput::Microphone,
        Some(p) => PlanInput::File(PathBuf::from(p)),
        None => PlanInput::Nothing,
    }
}

/// A Nooklet's input: the microphone, pasted text, a file, or nothing yet.
fn nooklet_input(input: Option<String>, microphone: bool, text: Option<String>) -> PlanInput {
    match text {
        Some(t) if !microphone && !t.trim().is_empty() => PlanInput::Text(t),
        _ => plan_input(input, microphone),
    }
}

/// Every language the pickers offer, by code and English name, and whether a voice speaks it
/// (only those are offered to translate into).
#[tauri::command]
pub fn flows_languages() -> Vec<Offered> {
    Voices::bundled().offered()
}

/// Every run, newest first.
#[tauri::command]
pub fn flows_runs(state: State<'_, AppState>) -> Vec<Run> {
    state.0.flows.runs()
}

/// What a run would do and still needs: for the file `input`, or the microphone.
#[tauri::command]
pub async fn flows_plan(
    state: State<'_, AppState>,
    input: Option<String>,
    microphone: bool,
    target: String,
    keep_voice: bool,
) -> CmdResult<Plan> {
    let flows = state.0.flows.clone();
    let input = plan_input(input, microphone);
    blocking(move || Ok(flows.plan(&input, &target, keep_voice))).await
}

/// Starts the download of everything the plan says is missing.
#[tauri::command]
pub async fn flows_install(
    state: State<'_, AppState>,
    input: Option<String>,
    microphone: bool,
    target: String,
    keep_voice: bool,
) -> CmdResult<()> {
    let flows = state.0.flows.clone();
    let input = plan_input(input, microphone);
    tauri::async_runtime::spawn_blocking(move || flows.start_install(&input, &target, keep_voice))
        .await
        .map_err(err)?
}

/// The downloads while they run, else null.
#[tauri::command]
pub fn flows_install_state(state: State<'_, AppState>) -> Option<Install> {
    state.0.flows.install()
}

#[tauri::command]
pub fn flows_cancel_install(state: State<'_, AppState>) {
    state.0.flows.cancel_install();
}

#[tauri::command]
pub fn flows_clear_install_error(state: State<'_, AppState>) {
    state.0.flows.clear_install_error();
}

/// Queues the translation of a file; `source` null lets Whisper tell the language.
#[tauri::command]
pub async fn flows_submit(
    state: State<'_, AppState>,
    input: String,
    source: Option<String>,
    target: String,
    keep_voice: bool,
) -> CmdResult<Run> {
    let flows = state.0.flows.clone();
    tauri::async_runtime::spawn_blocking(move || {
        flows.submit_file(
            &PathBuf::from(input),
            source.as_deref(),
            &target,
            keep_voice,
        )
    })
    .await
    .map_err(err)?
}

/// What a Nooklet's run (Transcribe, Summarize, Read aloud) would do and still needs: for the
/// file `input`, pasted `text`, or the microphone.
#[tauri::command]
pub async fn flows_plan_for(
    state: State<'_, AppState>,
    input: Option<String>,
    microphone: bool,
    text: Option<String>,
    order: Order,
) -> CmdResult<Plan> {
    let flows = state.0.flows.clone();
    let input = nooklet_input(input, microphone, text);
    blocking(move || Ok(flows.plan_for(&input, &order))).await
}

/// Starts the download of everything a Nooklet's plan says is missing.
#[tauri::command]
pub async fn flows_install_for(
    state: State<'_, AppState>,
    input: Option<String>,
    microphone: bool,
    text: Option<String>,
    order: Order,
) -> CmdResult<()> {
    let flows = state.0.flows.clone();
    let input = nooklet_input(input, microphone, text);
    tauri::async_runtime::spawn_blocking(move || flows.start_install_for(&input, &order))
        .await
        .map_err(err)?
}

/// Queues a Nooklet's run on a file or on pasted text.
#[tauri::command]
pub async fn flows_submit_for(
    state: State<'_, AppState>,
    input: Option<String>,
    text: Option<String>,
    order: Order,
) -> CmdResult<Run> {
    let flows = state.0.flows.clone();
    let input = nooklet_input(input, false, text);
    tauri::async_runtime::spawn_blocking(move || flows.submit_for(&input, &order))
        .await
        .map_err(err)?
}

/// Stops recording and queues the transcript of what was said.
#[tauri::command]
pub async fn flows_record_stop_for(state: State<'_, AppState>, order: Order) -> CmdResult<Run> {
    let recorder = state.0.recorder.clone();
    let wav = blocking(move || recorder.stop()).await?;
    let flows = state.0.flows.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let run = flows.submit_recording_for(&wav, &order);
        if run.is_err() {
            let _ = std::fs::remove_file(&wav);
        }
        run
    })
    .await
    .map_err(err)?
}

/// A document's or pasted text's language (when it can be told) and its words, before a run.
/// `id` is the page's number for it, to stop it with [`flows_peek_stop`].
#[tauri::command]
pub async fn flows_peek(
    state: State<'_, AppState>,
    input: Option<String>,
    text: Option<String>,
    id: Option<u64>,
) -> CmdResult<Peek> {
    let input = nooklet_input(input, false, text);
    Ok(state.0.flows.peek(&input, id).await)
}

/// The page no longer wants preview `id` (it left, or its input changed): it stops, unless
/// another preview waits for the same reading.
#[tauri::command]
pub fn flows_peek_stop(state: State<'_, AppState>, id: u64) {
    state.0.flows.stop_peek(id);
}

/// Opens the default microphone for a spoken run; levels go out on the "speech" topic.
#[tauri::command]
pub async fn flows_record_start(state: State<'_, AppState>) -> CmdResult<()> {
    let recorder = state.0.recorder.clone();
    blocking(move || recorder.start()).await
}

/// Stops recording and queues the translation of what was said.
#[tauri::command]
pub async fn flows_record_stop(
    state: State<'_, AppState>,
    source: Option<String>,
    target: String,
    keep_voice: bool,
) -> CmdResult<Run> {
    let recorder = state.0.recorder.clone();
    let wav = blocking(move || recorder.stop()).await?;
    let flows = state.0.flows.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let run = flows.submit_recording(&wav, source.as_deref(), &target, keep_voice);
        if run.is_err() {
            let _ = std::fs::remove_file(&wav);
        }
        run
    })
    .await
    .map_err(err)?
}

/// Drops the recording: nothing is translated.
#[tauri::command]
pub async fn flows_record_cancel(state: State<'_, AppState>) -> CmdResult<()> {
    let recorder = state.0.recorder.clone();
    blocking(move || {
        recorder.cancel();
        Ok(())
    })
    .await
}

/// Stops a queued or running run.
#[tauri::command]
pub fn flows_cancel(state: State<'_, AppState>, id: String) {
    state.0.flows.cancel(&id);
}

/// Removes a finished run and its files; false for an unknown or unfinished run.
#[tauri::command]
pub async fn flows_delete(state: State<'_, AppState>, id: String) -> CmdResult<bool> {
    let flows = state.0.flows.clone();
    blocking(move || Ok(flows.delete(&id))).await
}

/// The same run again: Run again and Try again.
#[tauri::command]
pub async fn flows_again(state: State<'_, AppState>, id: String) -> CmdResult<Run> {
    let flows = state.0.flows.clone();
    tauri::async_runtime::spawn_blocking(move || flows.again(&id))
        .await
        .map_err(err)?
}

/// Opens a run's folder in Explorer (the flows folder when `id` is null), creating it when needed.
#[tauri::command]
pub async fn flows_open_folder(state: State<'_, AppState>, id: Option<String>) -> CmdResult<()> {
    let flows = state.0.flows.clone();
    blocking(move || {
        let folder = match id {
            Some(id) => {
                flows
                    .run(&id)
                    .ok_or_else(|| anyhow::anyhow!("That run is gone."))?;
                flows.folder(&id)
            }
            None => flows.root().to_path_buf(),
        };
        std::fs::create_dir_all(&folder)?;
        tauri_plugin_opener::open_path(&folder, None::<&str>)?;
        Ok(())
    })
    .await
}

/// A finished run's file: its video, else its track.
fn run_file(state: &State<'_, AppState>, id: &str, video: bool) -> CmdResult<PathBuf> {
    let run = state
        .0
        .flows
        .run(id)
        .ok_or_else(|| "That run is gone.".to_string())?;
    let file = if video { run.video } else { run.audio };
    file.map(PathBuf::from)
        .ok_or_else(|| "That run has no file yet.".to_string())
}

/// Opens a finished run's video (or its track) in the default player.
#[tauri::command]
pub async fn flows_open(state: State<'_, AppState>, id: String, video: bool) -> CmdResult<()> {
    let file = run_file(&state, &id, video)?;
    blocking(move || Ok(tauri_plugin_opener::open_path(&file, None::<&str>)?)).await
}

/// Shows a finished run's track selected in Explorer.
#[tauri::command]
pub async fn flows_reveal(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let file = run_file(&state, &id, false)?;
    blocking(move || Ok(tauri_plugin_opener::reveal_item_in_dir(&file)?)).await
}

/// One of the files a run wrote (a transcript, a summary, a reading), and nothing else.
fn written_file(state: &State<'_, AppState>, id: &str, path: &str) -> CmdResult<PathBuf> {
    let run = state
        .0
        .flows
        .run(id)
        .ok_or_else(|| "That run is gone.".to_string())?;
    let wrote = run.files.iter().any(|f| f == path)
        || run.audio.as_deref() == Some(path)
        || run.video.as_deref() == Some(path);
    if !wrote {
        return Err("That is not one of the run's files.".into());
    }
    Ok(PathBuf::from(path))
}

/// Opens a file a run wrote in the program Windows opens it with.
#[tauri::command]
pub async fn flows_open_file(
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> CmdResult<()> {
    let file = written_file(&state, &id, &path)?;
    blocking(move || Ok(tauri_plugin_opener::open_path(&file, None::<&str>)?)).await
}

/// Shows a file a run wrote selected in Explorer.
#[tauri::command]
pub async fn flows_reveal_file(
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> CmdResult<()> {
    let file = written_file(&state, &id, &path)?;
    blocking(move || Ok(tauri_plugin_opener::reveal_item_in_dir(&file)?)).await
}
