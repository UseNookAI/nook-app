// No console window behind the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // macOS: Nook started again as the reaper of its engine processes (nook_core::process).
    #[cfg(unix)]
    if std::env::args().nth(1).as_deref() == Some(nook_core::process::REAPER_ARG) {
        nook_core::process::reaper_main();
    }
    nook_lib::run()
}
