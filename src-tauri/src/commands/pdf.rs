//! The PDF editor flow (`ui/src/api/pdf.ts`): PDFium's one download (the `pdfium` engine
//! component, `nook_core::pdf::PdfInstaller`, its progress on the "pdf" topic), and the open
//! documents (`nook_core::pdf::PdfEditor`): open, draw a page, pick
//! text, replace it, undo, save.
//!
//! A page comes back as raw PNG bytes (an `ArrayBuffer` on the page's side), not JSON.

use std::path::PathBuf;

use nook_core::flow::Install;
use nook_core::pdf::{Align, Area, Block, PdfDoc, Pick, Replaced};
use serde::Serialize;
use tauri::ipc::Response;
use tauri::State;

use super::{blocking, err, msg, CmdResult};
use crate::AppState;

/// Whether the PDF engine is in, its download's size, and the download while it runs.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfSetup {
    pub installed: bool,
    pub bytes: u64,
    pub install: Option<Install>,
}

#[tauri::command]
pub fn pdf_setup(state: State<'_, AppState>) -> PdfSetup {
    let setup = &state.0.pdf_setup;
    PdfSetup {
        installed: setup.installed(),
        bytes: setup.bytes(),
        install: setup.state(),
    }
}

/// Starts the PDF engine's download (about 4 MB); its progress goes out on the "pdf" topic.
#[tauri::command]
pub async fn pdf_install(state: State<'_, AppState>) -> CmdResult<()> {
    state.0.pdf_setup.start()
}

/// Stops the PDF engine's download.
#[tauri::command]
pub fn pdf_cancel_install(state: State<'_, AppState>) {
    state.0.pdf_setup.cancel();
}

/// Forgets a failed download, so the button comes back.
#[tauri::command]
pub fn pdf_clear_install_error(state: State<'_, AppState>) {
    state.0.pdf_setup.clear_error();
}

#[tauri::command]
pub async fn pdf_open(state: State<'_, AppState>, path: String) -> CmdResult<PdfDoc> {
    state.0.pdf.open(&PathBuf::from(path)).await.map_err(msg)
}

/// Every open document (the page opens again on the one it had).
#[tauri::command]
pub async fn pdf_docs(state: State<'_, AppState>) -> CmdResult<Vec<PdfDoc>> {
    state.0.pdf.docs().await.map_err(msg)
}

/// A page drawn `width` pixels wide, as PNG bytes.
#[tauri::command]
pub async fn pdf_render(
    state: State<'_, AppState>,
    id: String,
    page: u32,
    width: i32,
) -> CmdResult<Response> {
    let png = state.0.pdf.render(&id, page, width).await.map_err(msg)?;
    Ok(Response::new(png))
}

#[tauri::command]
pub async fn pdf_pick(
    state: State<'_, AppState>,
    id: String,
    page: u32,
    area: Area,
) -> CmdResult<Pick> {
    state.0.pdf.pick(&id, page, area).await.map_err(msg)
}

#[tauri::command]
pub async fn pdf_replace(
    state: State<'_, AppState>,
    id: String,
    block: Block,
    texts: Vec<String>,
    align: Align,
) -> CmdResult<Replaced> {
    state
        .0
        .pdf
        .replace(&id, block, texts, align)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn pdf_undo(state: State<'_, AppState>, id: String) -> CmdResult<PdfDoc> {
    state.0.pdf.undo(&id).await.map_err(err)
}

/// Saves to `path`, or beside the original as "<name> (edited).pdf" when null.
#[tauri::command]
pub async fn pdf_save(
    state: State<'_, AppState>,
    id: String,
    path: Option<String>,
) -> CmdResult<PdfDoc> {
    state
        .0
        .pdf
        .save(&id, path.map(PathBuf::from))
        .await
        .map_err(msg)
}

#[tauri::command]
pub async fn pdf_close(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.0.pdf.close(&id).await.map_err(msg)
}

/// Shows a saved PDF selected in Explorer.
#[tauri::command]
pub async fn pdf_reveal(path: String) -> CmdResult<()> {
    blocking(move || {
        Ok(tauri_plugin_opener::reveal_item_in_dir(PathBuf::from(
            path,
        ))?)
    })
    .await
}
