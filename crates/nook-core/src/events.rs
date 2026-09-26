//! Changes the UI must see. The Kotlin app used listeners (`CodeService.addListener`,
//! `RuntimeManager.addListener`) and Compose state; here every change is an [`Event`] on one
//! broadcast channel, which the Tauri shell forwards to the web UI as `nook://<topic>`.

use once_cell::sync::Lazy;
use serde::Serialize;
use tokio::sync::broadcast;

/// Well-known topics. The payload shape of each is documented where it is emitted.
pub mod topic {
    /// A runtime event (`RuntimeManager.RuntimeEvent`): load, unload, crash, engine install...
    pub const RUNTIME: &str = "runtime";
    /// Model or engine download progress.
    pub const DOWNLOADS: &str = "downloads";
    /// Code sessions changed (the Kotlin `CodeService` listener).
    pub const CODE: &str = "code";
    /// Update availability and installer download progress.
    pub const UPDATE: &str = "update";
    /// Video renders.
    pub const VIDEO: &str = "video";
    /// Flow runs and the downloads they need.
    pub const FLOWS: &str = "flows";
    /// The PDF editor's engine download.
    pub const PDF: &str = "pdf";
    /// Voice prompt: recording level and transcription.
    pub const SPEECH: &str = "speech";
    /// Settings changed.
    pub const SETTINGS: &str = "settings";
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub topic: String,
    pub payload: serde_json::Value,
}

static BUS: Lazy<broadcast::Sender<Event>> = Lazy::new(|| broadcast::channel(1024).0);

/// Sends an event to every subscriber. Never blocks; with nobody listening it is dropped.
pub fn emit(topic: &str, payload: impl Serialize) {
    let payload = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
    let _ = BUS.send(Event {
        topic: topic.to_string(),
        payload,
    });
}

/// A receiver for every event from now on.
pub fn subscribe() -> broadcast::Receiver<Event> {
    BUS.subscribe()
}
