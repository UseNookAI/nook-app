//! The runtime as a flow sees it: what is installed ([`Facts`]) and the few things a run asks of it
//! ([`FlowRuntime`]). [`RuntimeManager`] implements it; the service's tests use a scripted one.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::runtime::api::Priority;
use crate::runtime::{thinking, Backend, EngineComponent, Progress, RuntimeManager};

/// The speech model the flows listen with when it is installed, or download on an NVIDIA card:
/// Whisper's most accurate, which the flows' many languages want. Without CUDA Whisper runs on
/// the processor, where it is slow, so the catalog's default is downloaded there instead.
pub const FLOW_SPEECH_MODEL: &str = "whisper-large-v3-turbo";

/// What is installed and what is missing, for a plan and a run.
///
/// - `speech_model`: the installed speech model a run listens with (the most accurate one)
/// - `speech_download`: what to download when there is none: a catalog id and its size, with the
///   speech engine's when that is missing too
/// - `translator`: the chat model that translates, `(id, display name)`: the one chosen for Chat
///   and Code, so one choice serves everywhere, else the chat model
/// - `ffmpeg`: the runtime's FFmpeg, or one on the PATH
/// - `voice_engine`: audio.cpp's `audiocpp_cli.exe` when installed; `voice_backend` the build it
///   runs (`cuda`, `vulkan` or `cpu`)
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub speech_model: Option<String>,
    pub speech_engine_installed: bool,
    pub speech_download: Option<(String, u64)>,
    pub speech_engine_bytes: u64,
    pub translator: Option<(String, String)>,
    pub ffmpeg: Option<PathBuf>,
    pub ffmpeg_bytes: u64,
    pub voice_engine: Option<PathBuf>,
    pub voice_engine_bytes: u64,
    pub voice_backend: String,
    pub cpu: bool,
}

/// What a run asks of the runtime.
/// A turn with the graphics card to itself, for as long as `hold` lives.
pub struct Turn<'a> {
    pub hold: Box<dyn Send + 'a>,
    /// When the card still had less free than the turn asked for.
    pub short: Option<crate::runtime::Shortage>,
}

#[async_trait]
pub trait FlowRuntime: Send + Sync {
    fn facts(&self) -> Facts;

    /// Writes down a 16 kHz WAV: Whisper's verbose reply (`language`, and `segments` with `start`
    /// and `end` in seconds). `language` None lets Whisper tell.
    async fn transcribe(&self, wav: &Path, model_id: &str, language: Option<&str>)
        -> Result<Value>;

    /// The chat model's reply to one batch, thinking off and removed.
    async fn chat(&self, model_id: &str, system: &str, user: &str) -> Result<String>;

    /// Waits for the card (images, clips and every request on it use it too), frees every idle
    /// engine that is not pinned, and holds it until the returned turn is dropped; None when
    /// `cancel` fired first. The turn says when the card is still short of `need_bytes`.
    async fn gpu_turn<'a>(
        &'a self,
        need_bytes: u64,
        cancel: &CancellationToken,
    ) -> Option<Turn<'a>>;

    /// Downloads and installs an engine; false when stopped.
    async fn install_component(
        &self,
        component: EngineComponent,
        progress: Progress,
        cancel: &CancellationToken,
    ) -> Result<bool>;

    /// Downloads a catalog model, with its engine when missing; false when stopped.
    async fn download_model(
        &self,
        model_id: &str,
        progress: Progress,
        cancel: &CancellationToken,
    ) -> Result<bool>;
}

/// An `ffmpeg.exe` (`ffmpeg` on a Mac) on the PATH, as the original also took.
fn ffmpeg_on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(crate::process::exe("ffmpeg")))
        .find(|exe| exe.is_file())
}

#[async_trait]
impl FlowRuntime for RuntimeManager {
    fn facts(&self) -> Facts {
        let backend = self.backend_now();
        let registry = self.registry();
        let catalog = self.catalog();
        let packages = self.packages();
        let bytes = |c: EngineComponent| {
            packages
                .package_for(c, backend)
                .map(|p| p.total_bytes())
                .unwrap_or(0)
        };
        let installed = |c: EngineComponent| self.is_component_installed(c);
        let exe = |c: EngineComponent| {
            let exe = packages.executable(c, backend, c.executables());
            (installed(c) && exe.is_file()).then_some(exe)
        };

        let speech: Vec<_> = registry
            .list()
            .into_iter()
            .filter(|m| m.is_speech())
            .collect();
        let default_speech = catalog.default_speech_model();
        let speech_model = [Some(FLOW_SPEECH_MODEL), default_speech]
            .into_iter()
            .flatten()
            .find_map(|id| speech.iter().find(|m| m.id == id))
            .or_else(|| speech.first())
            .map(|m| m.id.clone());
        let speech_engine_installed = installed(EngineComponent::Whisper);
        let speech_engine_bytes = if speech_engine_installed {
            0
        } else {
            bytes(EngineComponent::Whisper)
        };
        let want = if backend == Backend::Cuda {
            Some(FLOW_SPEECH_MODEL)
        } else {
            default_speech
        };
        let speech_download = want
            .and_then(|id| catalog.find(id))
            .or_else(|| default_speech.and_then(|id| catalog.find(id)))
            .map(|m| (m.id.clone(), m.total_bytes() + speech_engine_bytes));

        let translator = registry
            .code_worker()
            .or_else(|| registry.worker_for("chat"))
            .map(|m| (m.id, m.display_name));

        let voice_backend = EngineComponent::Audio.runs_on(backend).id().to_string();
        Facts {
            speech_model,
            speech_engine_installed,
            speech_download,
            speech_engine_bytes,
            translator,
            ffmpeg: exe(EngineComponent::Ffmpeg).or_else(ffmpeg_on_path),
            ffmpeg_bytes: bytes(EngineComponent::Ffmpeg),
            voice_engine: exe(EngineComponent::Audio),
            voice_engine_bytes: bytes(EngineComponent::Audio),
            voice_backend,
            cpu: backend == Backend::Cpu,
        }
    }

    async fn transcribe(
        &self,
        wav: &Path,
        model_id: &str,
        language: Option<&str>,
    ) -> Result<Value> {
        self.transcribe_detailed(wav, Some(model_id), language)
            .await
    }

    async fn chat(&self, model_id: &str, system: &str, user: &str) -> Result<String> {
        let mut body = json!({
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
            "temperature": 0.2,
            "max_tokens": 2048,
        });
        thinking::apply(&mut body, model_id, false);
        let lease = self.acquire(model_id, Priority::Interactive).await?;
        let reply = lease.client().chat_text(body).await?;
        Ok(thinking::stripped(&reply))
    }

    async fn gpu_turn<'a>(
        &'a self,
        need_bytes: u64,
        cancel: &CancellationToken,
    ) -> Option<Turn<'a>> {
        let mut turn =
            RuntimeManager::gpu_turn(self, need_bytes, "the voice engine", cancel).await?;
        let short = turn.short.take();
        Some(Turn {
            hold: Box::new(turn),
            short,
        })
    }

    async fn install_component(
        &self,
        component: EngineComponent,
        progress: Progress,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let staged: crate::runtime::StagedProgress =
            std::sync::Arc::new(move |_stage: &str, done, total| progress(done, total));
        self.ensure_component(component, Some(staged), cancel).await
    }

    async fn download_model(
        &self,
        model_id: &str,
        progress: Progress,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        if self.catalog().find(model_id).is_none() {
            return Err(anyhow!("Unknown catalog model {model_id}"));
        }
        self.download(model_id, Some(progress), cancel).await
    }
}
