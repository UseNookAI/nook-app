//! The screen recorder Nooklet (`ui/src/api/capture.ts`): what can be recorded, a picture of it,
//! the sound's meters, recording and streaming (`nook_core::capture::CaptureService`, its state on
//! the "capture" topic), FFmpeg's download, stream keys, and the recorder's own windows:
//!
//! - the area picker, one see-through window over each screen, where the person drags over what
//!   to record (`capture_pick_area`, answered by `capture_area_picked`);
//! - the recording controls, a small bar on the recorded screen that stays on top and out of
//!   every capture, with the time, pause and stop.
//!
//! Both are the app's own page, told what to show by `window.__NOOK_VIEW__`.

use std::path::PathBuf;

use nook_core::capture::audio::AudioSource;
use nook_core::capture::service::Sources;
use nook_core::capture::sources::{self, OWN_TITLES};
use nook_core::capture::{CaptureState, Source, StartOptions};
use nook_core::events::{self, topic};
use nook_core::flow::Install;
use serde_json::json;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

use super::{blocking, err, msg, CmdResult};
use crate::AppState;

const BAR: &str = "capture-bar";
const PICKER: &str = "pick-area-";

/// The screens, windows and sound devices, and whether FFmpeg is in.
#[tauri::command]
pub async fn capture_sources(state: State<'_, AppState>) -> CmdResult<Sources> {
    let capture = state.0.capture.clone();
    blocking(move || Ok(capture.sources())).await
}

/// One frame of `source`, as a `data:` PNG.
#[tauri::command]
pub async fn capture_preview(state: State<'_, AppState>, source: Source) -> CmdResult<String> {
    state.0.capture.preview_url(&source).await.map_err(msg)
}

/// Shows how loud `audio` is (its levels on the "capture" topic) until told to stop.
#[tauri::command]
pub async fn capture_listen(state: State<'_, AppState>, audio: Vec<AudioSource>) -> CmdResult<()> {
    let capture = state.0.capture.clone();
    blocking(move || capture.listen(&audio)).await
}

/// Off the main thread: a Mac's sound capture takes a moment to stop.
#[tauri::command]
pub async fn capture_stop_listening(state: State<'_, AppState>) -> CmdResult<()> {
    let capture = state.0.capture.clone();
    blocking(move || {
        capture.stop_listening();
        Ok(())
    })
    .await
}

#[tauri::command]
pub fn capture_state(state: State<'_, AppState>) -> CaptureState {
    state.0.capture.state()
}

/// Starts recording or streaming; with `hide`, Nook's window goes down first, out of the way. The
/// recording controls come up on the recorded screen.
#[tauri::command]
pub async fn capture_start(
    app: AppHandle,
    state: State<'_, AppState>,
    options: StartOptions,
    hide: bool,
) -> CmdResult<CaptureState> {
    let main = app.get_webview_window("main");
    if hide {
        if let Some(m) = &main {
            let _ = m.minimize();
        }
    }
    match state.0.capture.start(options).await {
        Ok(started) => {
            open_bar(&app, started.monitor);
            Ok(started)
        }
        Err(e) => {
            restore_main(&app);
            Err(msg(e))
        }
    }
}

#[tauri::command]
pub async fn capture_pause(state: State<'_, AppState>) -> CmdResult<CaptureState> {
    state.0.capture.pause().await.map_err(msg)
}

#[tauri::command]
pub async fn capture_resume(state: State<'_, AppState>) -> CmdResult<CaptureState> {
    state.0.capture.resume().await.map_err(msg)
}

/// Stops (the recording's parts become one MP4), closes the controls and brings Nook back. Also
/// what the controls call once a recording ended by itself.
#[tauri::command]
pub async fn capture_stop(app: AppHandle, state: State<'_, AppState>) -> CmdResult<CaptureState> {
    let done = state.0.capture.stop().await.map_err(msg);
    if let Some(bar) = app.get_webview_window(BAR) {
        let _ = bar.close();
    }
    restore_main(&app);
    done
}

fn restore_main(app: &AppHandle) {
    if let Some(m) = app.get_webview_window("main") {
        let _ = m.unminimize();
        let _ = m.show();
        let _ = m.set_focus();
    }
}

/// The recording controls, top centre of the recorded screen, above everything and out of
/// every capture (this recording's included).
fn open_bar(app: &AppHandle, monitor: Option<u64>) {
    if app.get_webview_window(BAR).is_some() {
        return;
    }
    let built = WebviewWindowBuilder::new(app, BAR, WebviewUrl::App("index.html".into()))
        .title(OWN_TITLES[1])
        .inner_size(340.0, 52.0)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .visible(false)
        .initialization_script(r#"window.__NOOK_VIEW__ = { view: "capture-bar" };"#)
        .build();
    let bar = match built {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("The recording controls did not open: {e}");
            return;
        }
    };
    #[cfg(windows)]
    if let Ok(hwnd) = bar.hwnd() {
        sources::exclude_from_capture(hwnd.0 as isize);
    }
    // The NSWindow is only asked for and touched on the main thread, while the bar is there.
    #[cfg(target_os = "macos")]
    {
        let window = bar.clone();
        let _ = bar.run_on_main_thread(move || {
            if let Ok(ns_window) = window.ns_window() {
                sources::exclude_from_capture(ns_window as isize);
            }
        });
    }
    let screen = sources::screens()
        .into_iter()
        .find(|s| Some(s.handle) == monitor)
        .or_else(|| sources::screens().into_iter().find(|s| s.primary));
    // A Mac places windows in points, each screen at its own scale; the bar goes below its menu
    // bar.
    #[cfg(target_os = "macos")]
    if let Some(p) = screen.and_then(|s| nook_core::capture::mac::points(s.handle)) {
        let x = p.x + (p.width - 340.0) / 2.0;
        let _ = bar.set_position(tauri::LogicalPosition::new(x, p.y + 40.0));
    }
    #[cfg(not(target_os = "macos"))]
    if let (Some(s), Ok(size)) = (screen, bar.outer_size()) {
        let x = s.x + (s.width as i32 - size.width as i32) / 2;
        let _ = bar.set_position(tauri::PhysicalPosition::new(x, s.y + 12));
    }
    let _ = bar.show();
}

/// Puts a see-through picker over every screen for the person to drag over what to record;
/// Nook's window goes down meanwhile. The answer comes as `{"area": Source | null}` on the
/// "capture" topic.
#[tauri::command]
pub async fn capture_pick_area(app: AppHandle) -> CmdResult<()> {
    close_pickers(&app);
    if let Some(m) = app.get_webview_window("main") {
        let _ = m.minimize();
    }
    let screens = tauri::async_runtime::spawn_blocking(sources::screens)
        .await
        .map_err(err)?;
    if screens.is_empty() {
        // Nothing to pick on: Nook comes back and the page hears no area came.
        restore_main(&app);
        events::emit(topic::CAPTURE, json!({ "area": null }));
        return Ok(());
    }
    for (i, s) in screens.iter().enumerate() {
        let view = json!({
            "view": "pick-area",
            "screen": s.handle,
            "width": s.width,
            "height": s.height,
        });
        let picker = WebviewWindowBuilder::new(
            &app,
            format!("{PICKER}{i}"),
            WebviewUrl::App("index.html".into()),
        )
        .title(OWN_TITLES[0])
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .initialization_script(format!("window.__NOOK_VIEW__ = {view};"))
        .build()
        .map_err(err)?;
        #[cfg(target_os = "macos")]
        if let Some(p) = nook_core::capture::mac::points(s.handle) {
            let _ = picker.set_position(tauri::LogicalPosition::new(p.x, p.y));
            let _ = picker.set_size(tauri::LogicalSize::new(p.width, p.height));
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = picker.set_position(tauri::PhysicalPosition::new(s.x, s.y));
            let _ = picker.set_size(tauri::PhysicalSize::new(s.width, s.height));
        }
        let _ = picker.show();
        if s.primary {
            let _ = picker.set_focus();
        }
    }
    Ok(())
}

/// The area the person chose (None: they changed their mind): the pickers close, Nook comes
/// back and the page hears it.
#[tauri::command]
pub fn capture_area_picked(app: AppHandle, area: Option<Source>) {
    close_pickers(&app);
    restore_main(&app);
    events::emit(topic::CAPTURE, json!({ "area": area }));
}

fn close_pickers(app: &AppHandle) {
    for (label, w) in app.webview_windows() {
        if label.starts_with(PICKER) {
            let _ = w.close();
        }
    }
}

/// Starts FFmpeg's download.
#[tauri::command]
pub fn capture_install(state: State<'_, AppState>) -> CmdResult<()> {
    state.0.capture.start_install()
}

#[tauri::command]
pub fn capture_install_state(state: State<'_, AppState>) -> Option<Install> {
    state.0.capture.install_state()
}

#[tauri::command]
pub fn capture_cancel_install(state: State<'_, AppState>) {
    state.0.capture.cancel_install();
}

#[tauri::command]
pub fn capture_clear_install_error(state: State<'_, AppState>) {
    state.0.capture.clear_install_error();
}

/// The stream key kept for `service`, if one is.
#[tauri::command]
pub fn capture_stream_key(state: State<'_, AppState>, service: String) -> Option<String> {
    state.0.capture.stream_key(&service)
}

/// Keeps `key` for `service` (encrypted for this Windows account); None forgets it.
#[tauri::command]
pub fn capture_keep_stream_key(
    state: State<'_, AppState>,
    service: String,
    key: Option<String>,
) -> CmdResult<()> {
    state
        .0
        .capture
        .keep_stream_key(&service, key.as_deref())
        .map_err(msg)
}

fn saved(state: &State<'_, AppState>, path: &str) -> CmdResult<PathBuf> {
    let p = PathBuf::from(path);
    if !state.0.capture.is_saved(&p) {
        return Err(err("That is not a recording made here."));
    }
    Ok(p)
}

/// Opens a recording in the player Windows opens it with.
#[tauri::command]
pub async fn capture_open(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    let p = saved(&state, &path)?;
    blocking(move || Ok(tauri_plugin_opener::open_path(&p, None::<&str>)?)).await
}

/// Shows a recording selected in Explorer.
#[tauri::command]
pub async fn capture_reveal(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    let p = saved(&state, &path)?;
    blocking(move || Ok(tauri_plugin_opener::reveal_item_in_dir(&p)?)).await
}

/// Opens the folder recordings go to (`folder`, else the default one), making it first.
#[tauri::command]
pub async fn capture_open_folder(folder: Option<String>) -> CmdResult<()> {
    blocking(move || {
        let dir = folder
            .filter(|f| !f.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(nook_core::capture::service::default_folder);
        std::fs::create_dir_all(&dir)?;
        Ok(tauri_plugin_opener::open_path(&dir, None::<&str>)?)
    })
    .await
}
