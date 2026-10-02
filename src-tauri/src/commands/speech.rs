//! The voice prompt (`speech_*` in `ui/src/api/code.ts`): the microphone (`nook_core::speech`)
//! and its transcription by the runtime's speech engine (`CodeService::transcribe`: the default
//! speech model, the text stripped). Levels go out on the "speech" topic while recording.
//!
//! Opening the device, joining the recording thread and writing the WAV block briefly, so the
//! recorder runs on the blocking pool.

use nook_core::usage::Tool;
use tauri::State;

use super::{blocking, err, CmdResult};
use crate::AppState;

/// Opens the default microphone and starts recording (already recording is not an error).
#[tauri::command]
pub async fn speech_start(state: State<'_, AppState>) -> CmdResult<()> {
    let recorder = state.0.recorder.clone();
    blocking(move || recorder.start()).await
}

/// Stops recording and returns what was said, as text; the recording is deleted either way.
#[tauri::command]
pub async fn speech_stop_and_transcribe(state: State<'_, AppState>) -> CmdResult<String> {
    let recorder = state.0.recorder.clone();
    let wav = blocking(move || recorder.stop()).await?;
    let text = state.0.code.transcribe(&wav).await;
    if let Err(e) = tokio::fs::remove_file(&wav).await {
        tracing::warn!("Could not delete the recording {}: {e}", wav.display());
    }
    state.0.usage.outcome(Tool::VoicePrompt, &text);
    text.map_err(err)
}

/// Drops the recording: nothing is written.
#[tauri::command]
pub async fn speech_cancel(state: State<'_, AppState>) -> CmdResult<()> {
    let recorder = state.0.recorder.clone();
    blocking(move || {
        recorder.cancel();
        Ok(())
    })
    .await
}
