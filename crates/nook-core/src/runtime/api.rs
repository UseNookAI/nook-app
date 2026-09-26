//! The runtime as the Code worker and its sessions see it: the handful of `RuntimeManager` calls
//! `code/CodeService.java` makes. `RuntimeManager` implements [`WorkerRuntime`]; the code module
//! holds an `Arc<dyn WorkerRuntime>`, so it can be tested against a fake engine.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::inference_client::InferenceClient;
use super::model_catalog::ModelCatalog;
use super::model_registry::ModelRegistry;

/// Who asks (`RuntimeManager.Priority`): interactive requests go ahead of background ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Priority {
    Interactive,
    Background,
}

/// A model held loaded for one request (`RuntimeManager.Lease`). Dropping it lets the model go
/// idle again (the Java `close()`).
pub trait Lease: Send + Sync {
    fn client(&self) -> &InferenceClient;
}

#[async_trait]
pub trait WorkerRuntime: Send + Sync {
    fn registry(&self) -> Arc<ModelRegistry>;
    fn catalog(&self) -> Arc<ModelCatalog>;
    /// Whether an engine has the model loaded now (`engine(id).isPresent()`).
    fn is_loaded(&self, model_id: &str) -> bool;
    /// The context one request gets on the model's engine (the engine's own number while it
    /// runs), else what a load would plan; 0 when the model is not installed
    /// (`contextPerRequest`).
    fn context_per_request(&self, model_id: &str) -> u32;
    /// Loads the model when needed and holds it for one request (`acquire(id, priority)`).
    async fn acquire(&self, model_id: &str, priority: Priority) -> Result<Box<dyn Lease>>;
    /// Why speech cannot work now, or None (`speechProblem`).
    fn speech_problem(&self) -> Option<String>;
    /// Transcribes a WAV with the speech model, or the given one (`transcribe`).
    async fn transcribe(&self, wav: &Path, model_id: Option<&str>) -> Result<String>;
}
