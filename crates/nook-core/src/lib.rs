//! Nook's core: everything the app does apart from drawing it.
//!
//! The Tauri shell (`src-tauri`) owns one [`app::Nook`] and exposes its services to the web UI as
//! commands and events. Every module here ports a package of the Kotlin/Java app at 0.4.2.
//!
//! Conventions:
//! - IO is async on tokio; state behind `parking_lot` locks; errors are `anyhow::Result` with the
//!   same user-facing messages as the original (the UI shows them as they are).
//! - Types the UI sees derive `Serialize` with `#[serde(rename_all = "camelCase")]`, named after
//!   the Java records they replace, so the TypeScript types in `ui/src/api` mirror them.
//! - Child processes start through [`process`], so no console window flashes and they die with Nook.
//! - Changes the UI must see go out through [`events::emit`].

pub mod app;
pub mod build_info;
pub mod busy;
pub mod events;
pub mod gateway_port;
pub mod home;
pub mod logging;
pub mod migrate;
pub mod nooklets;
pub mod process;
pub mod resources;
pub mod settings;

pub mod code;
pub mod convert;
pub mod flow;
pub mod gateway;
pub mod ide;
pub mod pdf;
pub mod runtime;
pub mod speech;
pub mod update;
pub mod video;
pub mod web;
pub mod worker;

pub use app::Nook;
pub use home::Home;
