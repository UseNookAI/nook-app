//! The desktop shell: one window drawing the web UI in `ui/`, with `nook_core::Nook` behind it.
//! Commands live in `commands/<area>.rs` and are all listed in [`run`]; core events are forwarded
//! to the UI as `nook:<topic>`.
//!
//! The window starts hidden (`"visible": false` in tauri.conf.json): the UI sizes it for its first
//! screen and then shows it, so it never jumps from one size to another in view. Should the UI not
//! get that far (a script error, a dev server that is not up), Rust shows it after
//! [`SHOW_FALLBACK`] anyway.
//!
//! One Nook at a time (`nook_core::instance`): copies that start in the same moment wait for the
//! first one's window and hand over to it, and a Nook whose window never loads its page (WebView2
//! refusing it) hands over to a fresh copy of itself within [`WINDOW_DEADLINE`] rather than staying
//! on with nothing on screen.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use nook_core::instance::{self, Turn};
use nook_core::update::handover;
use nook_core::{Home, Nook};
use tauri::webview::PageLoadEvent;
use tauri::{Emitter, Manager};

mod commands;

pub struct AppState(pub Arc<Nook>);

/// How long the UI has to show the window before Rust shows it itself.
const SHOW_FALLBACK: Duration = Duration::from_secs(8);
/// How long the main window has to load its page before Nook counts it as failed.
const WINDOW_DEADLINE: Duration = Duration::from_secs(30);
/// How long a relaunched copy waits for the one it replaces to end.
const REPLACED_WAIT: Duration = Duration::from_secs(20);

/// Set once the main window has loaded its page.
static PAGE_LOADED: AtomicBool = AtomicBool::new(false);

pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    // Relaunched after a window that never loaded: the copy it replaces ends first.
    if let Some(pid) = instance::after_pid(&args) {
        instance::wait_for_exit(pid, REPLACED_WAIT);
    }
    let home = Home::resolve().expect("Nook cannot create its home folder");
    nook_core::logging::init(&home);
    // Started where the Kotlin Nook was installed (its updater's restart after installing this
    // app): the installed Nook takes over, and this process ends here.
    if handover::forward_to_installed() {
        return;
    }
    tracing::info!(
        "Nook {} starting in {}",
        nook_core::build_info::BuildInfo::current().label(),
        home.root().display()
    );
    let context = tauri::generate_context!();
    // HandOver: the single-instance plugin below gives this start to the running copy and ends it.
    if instance::wait_turn(&context.config().identifier) == Turn::GiveUp {
        tracing::warn!("Another Nook is starting but its window never came; this start ends");
        return;
    }
    let relaunched = instance::relaunched(&args);
    let nook = Nook::new(home).expect("Nook could not start");
    // An installed copy removes the Nook.exe the installer left there, once nothing needs it.
    if nook_core::update::install::relaunch_target().is_some() {
        handover::remove_leftovers(handover::old_program_dirs(), handover::handed_over());
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second start brings the running window forward instead.
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .on_page_load(|webview, payload| {
            if webview.label() == "main" && payload.event() == PageLoadEvent::Finished {
                PAGE_LOADED.store(true, Ordering::SeqCst);
            }
        })
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

            // A window that never loads its page would leave Nook on with nothing on screen, and
            // every later start handed to it: a fresh copy takes over instead, once; when that
            // one fails too, a message says so and it ends.
            let watched = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(WINDOW_DEADLINE).await;
                if PAGE_LOADED.load(Ordering::SeqCst) {
                    return;
                }
                if relaunched {
                    tracing::error!("The window did not load its page again; Nook ends");
                    let _ = tauri::async_runtime::spawn_blocking(|| {
                        instance::alert(
                            "Nook",
                            "Nook could not open its window. Restart Windows, then open Nook again.",
                        )
                    })
                    .await;
                } else {
                    tracing::error!(
                        "The window did not load its page within {}s; a fresh Nook takes over",
                        WINDOW_DEADLINE.as_secs()
                    );
                    if let Err(e) = instance::relaunch() {
                        tracing::error!("Could not start a fresh Nook: {e}");
                    }
                }
                watched.exit(0);
            });

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
            commands::usage::usage_overview,
            commands::usage::usage_set,
            commands::usage::usage_notice_seen,
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
        .build(context)
        .expect("error while building Nook")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let nook = app.state::<AppState>().0.clone();
                tauri::async_runtime::block_on(async move { nook.shutdown().await });
            }
        });
}
