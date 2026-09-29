//! The desktop shell: one window drawing the web UI in `ui/`, with `nook_core::Nook` behind it.
//! Commands live in `commands/<area>.rs` and are all listed in [`run`]; core events are forwarded
//! to the UI as `nook:<topic>`.
//!
//! The window starts hidden (`"visible": false` in tauri.conf.json): the UI sizes it for its first
//! screen and then shows it, so it never jumps from one size to another in view. Should the UI not
//! get that far (a script error, a dev server that is not up), Rust shows it after
//! [`SHOW_FALLBACK`] anyway.

use std::sync::Arc;
use std::time::Duration;

use nook_core::update::handover;
use nook_core::{Home, Nook};
use tauri::{Emitter, Manager};

mod commands;

pub struct AppState(pub Arc<Nook>);

/// How long the UI has to show the window before Rust shows it itself.
const SHOW_FALLBACK: Duration = Duration::from_secs(8);

pub fn run() {
    // First, before any thread: on a Mac, the PATH the person's shell has (for the worker's
    // toolchains), which an app opened from the Finder does not get.
    nook_core::platform::adopt_shell_path();
    let home = Home::resolve().expect("Nook cannot create its home folder");
    nook_core::logging::init(&home);
    // Started where the Kotlin Nook was installed (its updater's restart after installing this
    // app): the installed Nook takes over, and this process ends here. (Windows only: there never
    // was a Kotlin Nook on a Mac.)
    if cfg!(windows) && handover::forward_to_installed() {
        return;
    }
    tracing::info!(
        "Nook {} starting in {}",
        nook_core::build_info::BuildInfo::current().label(),
        home.root().display()
    );
    // Before any engine starts: on macOS the engines end with Nook through the reaper.
    nook_core::process::start_reaper();
    let nook = Nook::new(home).expect("Nook could not start");
    // An installed copy removes the Nook.exe the installer left there, once nothing needs it.
    if cfg!(windows) && nook_core::update::install::relaunch_target().is_some() {
        handover::remove_leftovers(handover::old_program_dirs(), handover::handed_over());
    }

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second start brings the running window forward instead.
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init());
    // A Mac's menu bar, whose Quit (Cmd+Q) asks first as closing does: the standard one ends the
    // app at once. Windows has no menu bar.
    #[cfg(target_os = "macos")]
    let builder = builder.menu(mac_menu).on_menu_event(|app, event| {
        if event.id() == QUIT_ITEM {
            ask_to_quit(app);
        }
    });
    builder
        .manage(AppState(nook.clone()))
        .setup(move |app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut rx = nook_core::events::subscribe();
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            let _ = handle.emit(&format!("nook:{}", ev.topic), ev.payload);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("UI event bridge skipped {n} events");
                        }
                        Err(_) => break,
                    }
                }
            });

            // The Video page plays clips from the home's videos folder and the Flows page its
            // tracks from the flows folder, through the asset protocol; tauri.conf.json covers the
            // default home, this covers NOOK_RS_HOME.
            for dir in [nook.home.videos_dir(), nook.home.flows_dir()] {
                if let Err(e) = app.asset_protocol_scope().allow_directory(&dir, true) {
                    tracing::warn!("Could not allow {} to the window: {e}", dir.display());
                }
            }

            // Once the updater has started the installer, the app quits so it can replace the files.
            let quitter = app.handle().clone();
            nook.updater.set_quit_hook(move || quitter.exit(0));

            if let Some(window) = app.get_webview_window("main") {
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(SHOW_FALLBACK).await;
                    if !window.is_visible().unwrap_or(true) {
                        tracing::warn!(
                            "The UI did not show the window within {}s; showing it",
                            SHOW_FALLBACK.as_secs()
                        );
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                });
            }

            let started = nook.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = started.start().await {
                    tracing::error!("background start failed: {e:#}");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app::app_info,
            commands::app::settings_all,
            commands::app::settings_set,
            commands::app::app_erase_everything,
            commands::app::app_quit,
            commands::app::app_quit_check,
            commands::app::app_eula,
            commands::app::app_notices,
            commands::update::update_status,
            commands::update::update_check,
            commands::update::update_start,
            commands::update::update_cancel,
            commands::update::update_snooze,
            commands::update::update_set_channel,
            commands::runtime::runtime_init,
            commands::runtime::runtime_refresh_models,
            commands::runtime::runtime_ensure_engine,
            commands::runtime::runtime_gpu_load,
            commands::runtime::runtime_status,
            commands::runtime::runtime_downloads,
            commands::runtime::runtime_unload,
            commands::runtime::runtime_pin,
            commands::runtime::runtime_load,
            commands::runtime::gpu_snapshot,
            commands::models::models_catalog,
            commands::models::models_installed,
            commands::models::models_downloads,
            commands::models::models_download,
            commands::models::models_pause,
            commands::models::models_resume,
            commands::models::models_cancel,
            commands::models::models_delete,
            commands::models::hub_search,
            commands::models::hub_variants,
            commands::models::hub_installed,
            commands::models::hub_download,
            commands::models::hub_cancel,
            commands::models::workers_preferences,
            commands::models::workers_current,
            commands::models::workers_set,
            commands::models::web_access_enabled,
            commands::models::web_access_set,
            commands::speech::speech_start,
            commands::speech::speech_stop_and_transcribe,
            commands::speech::speech_cancel,
            commands::ide::ide_load_prefs,
            commands::ide::ide_save_prefs,
            commands::ide::ide_resolve_folder,
            commands::ide::ide_branch,
            commands::ide::ide_list_dir,
            commands::ide::ide_read_file,
            commands::ide::ide_write_file,
            commands::ide::ide_file_times,
            commands::ide::ide_create_file,
            commands::ide::ide_create_folder,
            commands::ide::ide_rename,
            commands::ide::ide_delete,
            commands::code::code_snapshot,
            commands::code::code_start,
            commands::code::code_send,
            commands::code::code_stop,
            commands::code::code_apply,
            commands::code::code_discard,
            commands::code::code_undo,
            commands::code::code_delete,
            commands::code::code_rename,
            commands::code::code_run_diff,
            commands::code::code_next_context,
            commands::code::code_repository_state,
            commands::code::code_recent_repositories,
            commands::code::code_set_worker,
            commands::code::code_speech_problem,
            commands::code::code_speech_model,
            commands::code::code_speech_download,
            commands::code::code_speech_install,
            commands::video::video_clips,
            commands::video::video_submit,
            commands::video::video_cancel,
            commands::video::video_delete,
            commands::video::video_setup,
            commands::video::video_download_state,
            commands::video::video_download,
            commands::video::video_download_pause,
            commands::video::video_download_resume,
            commands::video::video_open_folder,
            commands::video::video_open,
            commands::video::video_reveal,
            commands::flows::flows_languages,
            commands::flows::flows_runs,
            commands::flows::flows_plan,
            commands::flows::flows_install,
            commands::flows::flows_install_state,
            commands::flows::flows_cancel_install,
            commands::flows::flows_clear_install_error,
            commands::flows::flows_submit,
            commands::flows::flows_record_start,
            commands::flows::flows_record_stop,
            commands::flows::flows_record_cancel,
            commands::flows::flows_cancel,
            commands::flows::flows_delete,
            commands::flows::flows_again,
            commands::flows::flows_open_folder,
            commands::flows::flows_open,
            commands::flows::flows_reveal,
            commands::flows::flows_plan_for,
            commands::flows::flows_install_for,
            commands::flows::flows_submit_for,
            commands::flows::flows_record_stop_for,
            commands::flows::flows_peek,
            commands::flows::flows_peek_stop,
            commands::flows::flows_open_file,
            commands::flows::flows_reveal_file,
            commands::pdf::pdf_setup,
            commands::pdf::pdf_install,
            commands::pdf::pdf_cancel_install,
            commands::pdf::pdf_clear_install_error,
            commands::pdf::pdf_open,
            commands::pdf::pdf_docs,
            commands::pdf::pdf_render,
            commands::pdf::pdf_pick,
            commands::pdf::pdf_replace,
            commands::pdf::pdf_undo,
            commands::pdf::pdf_save,
            commands::pdf::pdf_close,
            commands::pdf::pdf_reveal,
            commands::convert::convert_offer,
            commands::convert::convert_install,
            commands::convert::convert_install_state,
            commands::convert::convert_cancel_install,
            commands::convert::convert_clear_install_error,
            commands::convert::convert_start,
            commands::convert::convert_jobs,
            commands::convert::convert_cancel,
            commands::convert::convert_open,
            commands::convert::convert_reveal,
            commands::nooklets::nooklets_setup,
            commands::nooklets::nooklets_find,
            commands::nooklets::nooklets_install,
            commands::nooklets::nooklets_cancel_install,
            commands::nooklets::nooklets_clear_install_error,
            commands::capture::capture_sources,
            commands::capture::capture_preview,
            commands::capture::capture_listen,
            commands::capture::capture_stop_listening,
            commands::capture::capture_state,
            commands::capture::capture_start,
            commands::capture::capture_pause,
            commands::capture::capture_resume,
            commands::capture::capture_stop,
            commands::capture::capture_pick_area,
            commands::capture::capture_area_picked,
            commands::capture::capture_install,
            commands::capture::capture_install_state,
            commands::capture::capture_cancel_install,
            commands::capture::capture_clear_install_error,
            commands::capture::capture_stream_key,
            commands::capture::capture_keep_stream_key,
            commands::capture::capture_open,
            commands::capture::capture_reveal,
            commands::capture::capture_open_folder,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Nook")
        .run(|app, event| match event {
            // Quit from the menu or the Dock (Cmd+Q; code None) goes through the window's own
            // close, which asks first while something is running and then quits with a code.
            // An exit nobody asked for with a code, while the window is still there: the window's
            // own close asks first, then quits with one.
            tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } if cfg!(target_os = "macos") => {
                if app.get_webview_window("main").is_some() {
                    api.prevent_exit();
                    ask_to_quit(app);
                }
            }
            // The Dock's icon clicked with the window put away: it comes back.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen {
                has_visible_windows: false,
                ..
            } => {
                if let Some(main) = app.get_webview_window("main") {
                    let _ = main.unminimize();
                    let _ = main.show();
                    let _ = main.set_focus();
                }
            }
            tauri::RunEvent::Exit => {
                let nook = app.state::<AppState>().0.clone();
                tauri::async_runtime::block_on(async move { nook.shutdown().await });
            }
            _ => {}
        });
}

/// The Mac menu's Quit.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const QUIT_ITEM: &str = "nook-quit";

/// Hands a quit to the window, which asks first while something runs and then quits; with no
/// window to ask, quits.
fn ask_to_quit<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    match app.get_webview_window("main") {
        Some(main) => {
            let _ = main.emit("nook:quit-requested", ());
        }
        None => app.exit(0),
    }
}

/// The Mac's menu bar: the app's menu (About, Services, Hide, Quit), Edit (so copy and paste
/// work in the page's fields) and Window. Quit is Nook's own item, not the standard one.
#[cfg(target_os = "macos")]
fn mac_menu<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
    let info = app.package_info();
    let about = AboutMetadata {
        name: Some(info.name.clone()),
        version: Some(info.version.to_string()),
        ..Default::default()
    };
    let nook = Submenu::with_items(
        app,
        info.name.clone(),
        true,
        &[
            &PredefinedMenuItem::about(app, None, Some(about))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, QUIT_ITEM, "Quit Nook", true, Some("CmdOrCtrl+Q"))?,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;
    Menu::with_items(app, &[&nook, &edit, &window])
}
