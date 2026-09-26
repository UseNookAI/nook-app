//! Self-update. Ports `ai.nook.agent.update` (ReleaseManifest, UpdateSource) and
//! `service/VersionUpdateService.kt`. BuildInfo is `crate::build_info`, BusyWork is `crate::busy`.
//!
//! - [`manifest`]: the signed `latest.json` and its Ed25519 signature (`ReleaseManifest.java`).
//! - [`source`]: where manifests come from, the channel, what counts as newer, the installer's
//!   sha256 and size check (`UpdateSource.java`).
//! - [`updater`]: the scheduled check, the offer, the download with progress, cancel and "Later",
//!   the dev channel's automatic install once nothing is busy (`VersionUpdateService.kt`).
//! - [`install`]: running the NSIS setup silently after the app quits and starting the new build.
//! - [`handover`]: the `Nook.exe` the installer leaves where the Kotlin Nook was installed, for its
//!   updater's restart: it starts the installed Nook, which removes it later.
//!
//! Wiring (the Tauri shell): build one [`Updater`] with
//! `Updater::new(UpdateSource::from_env(Some(settings)), home.temp_dir().join("update"))`, register
//! every [`crate::busy::BusyWork`] with [`Updater::register_busy`], set
//! [`Updater::set_quit_hook`] to `AppHandle::exit(0)`, call [`Updater::start`] inside the runtime
//! and [`Updater::check_for_updates`] once the Hub is up; forward `update` events to the UI.

pub mod handover;
pub mod install;
pub mod manifest;
pub mod source;
pub mod updater;

pub use manifest::Release;
pub use source::{ChannelStore, UpdateSource, DEV, STABLE};
pub use updater::{due_for_check, wait_note, UpdateStatus, Updater};

#[cfg(test)]
mod tests;
