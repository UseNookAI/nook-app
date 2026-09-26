//! Tauri commands by area. Names are `<area>_<action>`; arguments and results are camelCase JSON
//! mirroring the contracts in `ui/src/api/<area>.ts`. Errors are the user-facing message as a
//! string.

pub mod app;
pub mod code;
pub mod flows;
pub mod ide;
pub mod models;
pub mod pdf;
pub mod runtime;
pub mod speech;
pub mod update;
pub mod video;

/// A command's error: the message the UI shows as it is.
pub type CmdResult<T> = Result<T, String>;

/// The error's own message ("Unknown catalog model x"): for errors written for the user.
pub fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// The message with its causes ("Could not open a.txt: Access is denied."): for errors that wrap
/// an OS or network failure.
pub fn msg(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// Runs file or device work on the blocking pool so it never holds up the window; the error is
/// [`msg`].
pub async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
        .map_err(msg)
}
