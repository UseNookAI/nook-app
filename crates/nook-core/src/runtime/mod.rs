//! The local inference runtime. Ports `ai.nook.agent.runtime` (RuntimeManager, EngineProcess,
//! EnginePackages, Downloader, ModelCatalog, ModelRegistry, GgufMetadata, Footprint, EvictionPlan,
//! GpuInventory, HardwareMonitor, HuggingFaceHub, InferenceClient, SpeedProbe, Thinking,
//! WhisperProcess, ImageEngine, VideoEngine, Backend, EngineComponent, RuntimeConfig) and
//! `service/ModelDownloadService.kt`.
//!
//! # The base (wave 1)
//!
//! Everything below is plain services with no engine process of its own; the runtime manager,
//! the engine processes and the speed probe are built on them.
//!
//! - [`RuntimeConfig`] (`config.rs`): builds one of each service for a [`Home`](crate::Home) and
//!   shares them as `Arc`s: `RuntimeConfig::new(home)?`.
//! - [`Backend`] / [`EngineComponent`]: which engine build (`cuda`, `vulkan`, `cpu`) and which
//!   engine (`llama`, `whisper`, `sd`); `EngineComponent::for_task(task)`.
//! - [`EnginePackages`]: the pinned engines in `runtime/engines.json`. `package_for`, `dir`,
//!   `server_executable`, `executable(component, backend, &[candidates])`,
//!   `is_installed(component, backend)`, and
//!   `ensure_installed(component, backend, Option<StagedProgress>, &CancellationToken).await`
//!   (download, verify, extract into `runtime\bin\<backend>[\<component>]`).
//! - [`Downloader`]: `download(url, target, sha256, expected_bytes, Option<&Progress>, &cancel)
//!   .await -> Outcome` with `.part` files, Range resume and sha256 verification.
//! - [`ModelCatalog`]: the curated models (`runtime/catalog.json`): `all`, `find`,
//!   `find_by_file`, `worker_models`, `recommend_chat`, the default model per task.
//! - [`ModelRegistry`]: the models on disk ([`LocalModel`]), from this home's `models` folder and,
//!   read-only, the installed Nook's (`LocalModel::shared`). `list`, `find`, `first_for_task`,
//!   `worker_for`, `code_worker`, `worker_preferences` / `set_worker_preference`
//!   (`runtime\workers.json`), `download(id, progress, &cancel).await`, `delete(id)` (refuses a
//!   shared model). A GGUF whose header is no language model's is listed with the task
//!   [`UNSUPPORTED`] and why (`LocalModel::unsupported`), so no task picks it.
//! - [`HuggingFaceHub`]: the Browse page: `search(query, limit).await`, `variants(repo).await`,
//!   `download(repo, variant, progress, &cancel).await`, `is_installed`, and the pure helpers
//!   `fit`, `model_id`, `display_name`, `size`, `quant_of`.
//! - [`GgufMetadata`]: `GgufMetadata::read(path)` reads a model's header (layers, heads, context,
//!   experts) without touching the tensors; `not_a_language_model()` tells a model from the files
//!   beside one (projectors, encoders, prediction heads).
//! - [`Footprint`] and [`eviction_plan::victims`]: the memory plan for a model at a context size,
//!   and which idle engines to evict for an incoming one.
//! - [`GpuInventory`]: GPUs from `nvidia-smi`, else from the Vulkan engine's `--list-devices`.
//!   Share as `Arc<GpuInventory>`; `snapshot().await` (cached, for the status bar),
//!   `refresh().await`, `budget_bytes().await`, `select_backend().await`, `tensor_split()`,
//!   `set_vulkan_engine(locator)`.
//! - [`HardwareMonitor`]: CPU, memory and the GPU snapshot for the status bar.
//! - [`InferenceClient`]: one llama-server over HTTP: `health`, `props`, `chat`, `completion`,
//!   `chat_text`, `chat_stream(body, on_delta, &cancel)` (returns the usage), `embeddings`, and
//!   `forward(path, body, stream) -> reqwest::Response` for the gateway.
//! - [`thinking`]: whether a model thinks (`apply`, `apply_to_constrained`, `stripped`,
//!   `EnvelopeFilter`, `ran_out_while_thinking`).
//!
//! # The engines and the manager (wave 2)
//!
//! - [`EngineProcess`] (`engine_process.rs`): one llama-server on a random loopback port with its
//!   own API key; [`Plan`] (GPU layers, context per request, slots, KV type, split, embedding,
//!   experts in RAM) and `command(...)`; `start(timeout).await` (health wait, `/props` read),
//!   `stop().await` (kills the process tree), `state()` ([`EngineState`]), `is_alive()`,
//!   `context_mismatch()`, `log_tail(n)`; logs in `runtime\logs\<model>.log`. An engine that
//!   exits while it starts fails with [`EngineExited`], carrying the [`LoadFailure`] its log names.
//! - [`WhisperProcess`]: one whisper-server; `start`, `transcribe(wav, language)`,
//!   `inference(wav, language, "json" | "verbose_json")`, `stop`.
//! - [`SpeedProbe`]: the first-load speed measurement kept in `runtime\probe.json`
//!   (`current(model, pin, driver)`, `measure(client, ...)`, `all()`); under
//!   [`speed_probe::MIN_WORKER_TPS`] a model is too slow for Nook Code.
//! - [`ImageEngine`] and [`VideoEngine`]: one sd.cpp process per image or clip;
//!   `request_for(catalog defaults, ...)`, `command(...)`, `generate(...).await`. A clip reports
//!   [`VideoStage`]s through a [`VideoProgress`] and can be stopped ([`video_engine::Stopped`]).
//! - [`RuntimeManager`] (`manager.rs`): the resident set of engines. `RuntimeManager::new(config)`
//!   then `start().await` (its module docs give the app's start-up order); `status().await`
//!   ([`Status`]), `acquire(model, Priority).await` ([`Lease`], released on drop), `load`,
//!   `unload`, `pin`, `context_per_request`, `gpu_load().await`, `readiness()`, speech
//!   (`speech_problem`, `transcribe`, `transcribe_detailed`), `generate_image`, `generate_video`,
//!   downloads (`download`, `download_async`, `download_hub_async`, `cancel_hub_download`,
//!   `downloads()`), `recent_events()`, `shutdown().await`. It implements
//!   [`api::WorkerRuntime`], [`manager::VideoRuntime`] and [`BusyWork`](crate::busy::BusyWork).
//!   A model the engine cannot load fails `acquire` and `load` with a [`ModelLoadError`]; one it
//!   refused for good is not started again until its file or the engine changes.
//! - [`ModelDownloadService`] (`downloads.rs`): Settings › Models' downloads (launch, pause,
//!   resume, stop, delete, refresh) over the manager, with [`DownloadState`] and
//!   [`download_lines`](downloads::download_lines).
//!
//! Events: every [`RuntimeEvent`] goes out on `topic::RUNTIME` as
//! `{"kind","modelId","detail","at"}` (the last 200 are kept for the Runtime page). Downloads go
//! out on `topic::DOWNLOADS` as `{"source":"runtime","downloads":{key: 0..1}}` from the manager and
//! `{"source":"library", ...DownloadState}` from the download service.
//!
//! # Progress and cancellation
//!
//! Long work reports through [`Progress`], an `Arc<dyn Fn(done, total) + Send + Sync>` called on
//! the downloading task (keep it cheap: store a number, emit an event), with `total` 0 while
//! unknown. Engine installs use [`StagedProgress`], the same with a stage (`"download"`,
//! `"extract"`). Cancellation is a `tokio_util::sync::CancellationToken` (the original's
//! `BooleanSupplier`); a cancelled download keeps its `.part` file and resumes next time.
//!
//! # Serialized shapes
//!
//! Types the UI shows derive `Serialize` with camelCase fields named as the Java records
//! (`CatalogModel`, `Artifact`, `LocalModel`, `GpuDevice`, `Metrics`, `Snapshot`, `Repo`,
//! `Variant`, `HubFile`, `Reading`, `Footprint`). Enums serialize as the Java constant names
//! (`"CUDA"`, `"NVIDIA_SMI"`, `"NO_GPU"`), as Jackson wrote them. Byte counts that the original
//! kept as -1 when unknown are 0 here.

use std::sync::Arc;

pub mod api;
pub mod backend;
pub mod config;
pub mod downloader;
pub mod downloads;
pub mod engine_component;
pub mod engine_packages;
pub mod engine_process;
pub mod eviction_plan;
pub mod footprint;
pub mod gguf_metadata;
pub mod gpu_inventory;
pub mod hardware_monitor;
pub mod hugging_face_hub;
pub mod image_engine;
pub mod inference_client;
pub mod manager;
pub mod model_catalog;
pub mod model_registry;
pub mod speed_probe;
pub mod thinking;
pub mod video_engine;
pub mod whisper_process;

pub use backend::Backend;
pub use config::RuntimeConfig;
pub use downloader::{Downloader, Outcome};
pub use downloads::{AiModelDto, DownloadLine, DownloadState, ModelDownloadService, PromptModel};
pub use engine_component::EngineComponent;
pub use engine_packages::{EngineArtifact, EnginePackages, Package};
pub use engine_process::{EngineExited, EngineProcess, LoadFailure, Plan, State as EngineState};
pub use footprint::Footprint;
pub use gguf_metadata::{GgufMetadata, GgufValue};
pub use gpu_inventory::{GpuDevice, GpuInventory, Metrics, Snapshot, Source, DRIVER_RESERVE_BYTES};
pub use hardware_monitor::{HardwareMonitor, Reading};
pub use hugging_face_hub::{Fit, HubFile, HuggingFaceHub, Repo, Variant};
pub use image_engine::{ImageEngine, ImageRequest, ImageResult};
pub use inference_client::InferenceClient;
pub use manager::{
    AdmissionError, EngineInfo, Lease, ModelLoadError, Readiness, RuntimeEvent, RuntimeManager,
    RuntimeOptions, SpeechInfo, Status,
};
pub use model_catalog::{Artifact, CatalogModel, ModelCatalog};
pub use model_registry::{LocalModel, ModelRegistry, CODE_WORKER, UNSUPPORTED};
pub use speed_probe::{ProbeResult, SpeedProbe};
pub use video_engine::{
    Stage as VideoStage, VideoEngine, VideoProgress, VideoRequest, VideoResult,
};
pub use whisper_process::WhisperProcess;

/// Progress of long work: `(bytes_done, bytes_total)`, total 0 while unknown.
pub type Progress = Arc<dyn Fn(u64, u64) + Send + Sync>;

/// Progress with a stage name: `(stage, bytes_done, bytes_total)`.
pub type StagedProgress = Arc<dyn Fn(&str, u64, u64) + Send + Sync>;

pub(crate) fn report(progress: Option<&Progress>, done: u64, total: u64) {
    if let Some(p) = progress {
        p(done, total);
    }
}
