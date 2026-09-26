//! Ports `runtime/RuntimeManager.java`.
//!
//! Owns the resident set of engines on the GPU. Decides placement (how many layers go to the
//! GPU), admits requests (loading and evicting as needed), keeps engines healthy and evicts idle
//! ones. Text and embedding models run as llama-server processes, speech as whisper-server
//! processes, and images and videos as one-shot stable-diffusion.cpp runs that hold the GPU only
//! while rendering. See docs/architecture/local-inference-runtime.md section 5 of the original.
//!
//! # Creating and starting it
//!
//! The original was a Spring bean (`RuntimeConfig.runtimeManager`, `destroyMethod = "shutdown"`,
//! plus a JVM shutdown hook) whose constructor pointed the GPU inventory at the Vulkan engine and
//! scheduled the idle sweep (every 5 s) and the resume of interrupted downloads (after 20 s). Here:
//!
//! 1. `let runtime = RuntimeManager::new(RuntimeConfig::new(home)?);` when the app's services are
//!    built (no process, no network; the inventory starts its first reading in the background).
//! 2. `runtime.start().await` once the window is up: selects the backend and starts the sweep
//!    and the resume timer.
//! 3. HubScreen's first-start work, in this order: `let needs = runtime.needs_engine().await;`,
//!    then `downloads.refresh_sync().await` (the [`ModelDownloadService`]), then, when `needs`,
//!    `runtime.install_missing_engine().await` in the background and show the note it returns.
//!    A first start with no model installed does not download the runtime: the first model
//!    download installs it.
//! 4. `runtime.shutdown().await` before exit. Engines also die with the app on their own (they
//!    are in the app's kill-on-close job and killed when dropped).
//!
//! [`ModelDownloadService`]: super::downloads::ModelDownloadService

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

use super::api::{self, Priority, WorkerRuntime};
use super::backend::Backend;
use super::config::RuntimeConfig;
use super::downloader::part_path;
use super::engine_component::EngineComponent;
use super::engine_packages::EnginePackages;
use super::engine_process::{EngineExited, EngineProcess, LoadFailure, Plan, State};
use super::eviction_plan;
use super::footprint::Footprint;
use super::gpu_inventory::{self, GpuDevice, GpuInventory, BACKEND_ENV};
use super::hugging_face_hub::{self, HuggingFaceHub, Repo, Variant};
use super::image_engine::{ImageEngine, ImageResult};
use super::inference_client::InferenceClient;
use super::model_catalog::ModelCatalog;
use super::model_registry::{LocalModel, ModelRegistry};
use super::speed_probe::{ProbeResult, SpeedProbe, MIN_WORKER_TPS};
use super::video_engine::{Stopped, VideoEngine, VideoProgress, VideoResult};
use super::whisper_process::WhisperProcess;
use super::{Progress, StagedProgress};
use crate::busy::BusyWork;
use crate::events::{self, topic};

/// The most recent events kept for the Settings runtime page.
pub const RECENT_EVENTS_MAX: usize = 200;
/// Events the status carries.
pub const STATUS_EVENTS: usize = 40;
/// The least load reported while Nook is at work, so the mark still turns while the card reads
/// little.
pub const MIN_BUSY_LOAD: f64 = 0.2;
/// Experts in RAM need room beside the desktop; below this the runtime pages layers instead.
pub const MIN_RAM_FOR_EXPERTS: u64 = 24 << 30;
/// Catalog and Hub downloads that run at once; more wait their turn.
const DOWNLOAD_WORKERS: usize = 2;
/// How often a download reports progress as a runtime event.
const PROGRESS_EVENT_EVERY: Duration = Duration::from_secs(2);
/// How often download progress goes to the UI on the `downloads` topic.
const PROGRESS_UI_EVERY: Duration = Duration::from_millis(250);

/// Whether the runtime can serve a chat now. Serialized as the Java constant name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Readiness {
    EngineMissing,
    NoModels,
    Ready,
}

/// A model could not be admitted (`RuntimeManager.AdmissionException`); the message is written
/// for the user. Tell it apart with `err.is::<AdmissionError>()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionError(pub String);

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AdmissionError {}

/// A model the engine cannot load, in words for the person ("X can't run in Nook: the engine
/// (llama.cpp b10752) doesn't know its model architecture 'y'."). `permanent`: starting the
/// engine again cannot help until the model file or the engine changes, so meanwhile the runtime
/// refuses the model at once instead of starting it again for every request. Tell it apart with
/// `err.is::<ModelLoadError>()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelLoadError {
    pub message: String,
    pub permanent: bool,
}

impl ModelLoadError {
    /// The error for `model` (its name) that `engine` ("llama.cpp b10752") gave up on while it
    /// started.
    pub fn from_engine(model: &str, engine: &str, cause: &LoadFailure) -> ModelLoadError {
        let message = match cause {
            LoadFailure::UnknownArchitecture(arch) => format!(
                "{model} can't run in Nook: the engine ({engine}) doesn't know its model architecture '{arch}'."
            ),
            LoadFailure::OutOfMemory => format!(
                "{model} could not be loaded: the GPU ran out of memory. Close other applications that use it, or pick a smaller model."
            ),
            LoadFailure::Refused(Some(why)) => {
                let why = if why.chars().count() > 160 {
                    format!("{}...", why.chars().take(157).collect::<String>())
                } else {
                    why.clone()
                };
                format!("{model} can't run in Nook: the engine ({engine}) could not load it ({why}).")
            }
            LoadFailure::Refused(None) => format!(
                "{model} could not be loaded: the engine ({engine}) stopped while it loaded it."
            ),
        };
        ModelLoadError {
            message,
            permanent: cause.permanent(),
        }
    }
}

impl std::fmt::Display for ModelLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModelLoadError {}

/// A model file or an engine as a refusal saw it: path, size and time of the last change.
type FileStamp = (PathBuf, u64, Option<SystemTime>);

fn file_stamp(p: &Path) -> FileStamp {
    let meta = std::fs::metadata(p).ok();
    (
        p.to_path_buf(),
        meta.as_ref().map(|m| m.len()).unwrap_or(0),
        meta.and_then(|m| m.modified().ok()),
    )
}

/// A model the engine refused for good, with the model file and the engine as they were then.
struct Refusal {
    stamp: [FileStamp; 2],
    error: ModelLoadError,
}

/// Something the runtime did, for the event feed (`RuntimeManager.RuntimeEvent`). Published on
/// [`topic::RUNTIME`] as `{"kind","modelId","detail","at"}`.
///
/// Kinds: `model_loading`, `model_loaded`, `model_failed`, `model_evicted`, `engine_crashed`,
/// `engine_installing`, `engine_installed`, `context_mismatch`, `ctx_override`, `slots_override`,
/// `partial_offload`, `experts_in_ram`, `cpu_fallback`, `integrated_graphics`, `probe_measured`,
/// `download_progress`, `download_resumed`, `model_downloaded`, `download_cancelled`,
/// `download_failed`, `image_rendering`, `image_rendered`, `image_tight_vram`,
/// `video_rendering`, `video_rendered`, `video_tight_vram`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEvent {
    pub kind: String,
    pub model_id: Option<String>,
    pub detail: Option<String>,
    pub at: DateTime<Utc>,
}

/// One loaded text engine as the Runtime page shows it. `ctx_per_slot`: context tokens one
/// request sees (the engine's own number once it has reported it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    pub model_id: String,
    pub display_name: String,
    pub state: State,
    pub port: u16,
    pub gpu_layers: i32,
    pub layers: u32,
    pub ctx_per_slot: u32,
    pub slots: u32,
    pub in_flight: u32,
    pub pinned: bool,
    pub started_at: Option<DateTime<Utc>>,
    pub last_used: DateTime<Utc>,
    pub failure: Option<String>,
    pub active: u32,
    pub waiting_interactive: u32,
    pub waiting_background: u32,
    pub tensor_split: Option<String>,
}

/// One loaded speech engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechInfo {
    pub model_id: String,
    pub port: u16,
    pub in_flight: u32,
    pub last_used: DateTime<Utc>,
}

/// Everything the Runtime and Models pages show (`RuntimeManager.Status`). `downloads` maps a
/// catalog model id, or a [`RuntimeManager::hub_key`], to its progress from 0 to 1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub backend: Backend,
    pub engine_version: String,
    pub engine_installed: bool,
    pub speech_engine_installed: bool,
    pub image_engine_installed: bool,
    pub devices: Vec<GpuDevice>,
    pub budget_bytes: u64,
    pub engines: Vec<EngineInfo>,
    pub speech_engines: Vec<SpeechInfo>,
    pub image_busy: bool,
    pub installed_models: Vec<String>,
    pub readiness: Readiness,
    pub speech_engine_version: String,
    pub image_engine_version: String,
    pub downloads: BTreeMap<String, f64>,
    pub recent_events: Vec<RuntimeEvent>,
    pub probes: Vec<ProbeResult>,
}

/// The runtime's downloads as they change, on [`topic::DOWNLOADS`]:
/// `{"source":"runtime","downloads":{"<id or hub key>":0.42}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDownloads {
    pub source: String,
    pub downloads: BTreeMap<String, f64>,
}

/// Where an engine's executable is, for a component and backend.
pub type ExecutableLocator = Arc<dyn Fn(EngineComponent, Backend) -> PathBuf + Send + Sync>;

/// Timings and overrides. The defaults are the original's; the other fields exist for tests and
/// QA.
#[derive(Clone)]
pub struct RuntimeOptions {
    /// An engine unused this long is unloaded (not when pinned).
    pub idle_ttl: Duration,
    /// How long a text engine may take to become healthy.
    pub start_timeout: Duration,
    /// How long a speech engine may take to answer.
    pub speech_start_timeout: Duration,
    /// How often crashed and idle engines are looked for.
    pub sweep_every: Duration,
    /// When interrupted downloads are picked up after start.
    pub resume_after: Duration,
    /// How long the speed probe waits behind the load that scheduled it.
    pub probe_delay: Duration,
    /// A fixed backend instead of detection (`NOOK_RS_BACKEND` also overrides detection).
    pub backend: Option<Backend>,
    /// Other engine executables than the installed ones.
    pub executables: Option<ExecutableLocator>,
    /// The machine's memory instead of reading it.
    pub system_ram_bytes: Option<u64>,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        RuntimeOptions {
            idle_ttl: Duration::from_secs(10 * 60),
            start_timeout: Duration::from_secs(5 * 60),
            speech_start_timeout: Duration::from_secs(2 * 60),
            sweep_every: Duration::from_secs(5),
            resume_after: Duration::from_secs(20),
            probe_delay: Duration::from_secs(2),
            backend: None,
            executables: None,
            system_ram_bytes: None,
        }
    }
}

// ------------------------------------------------------------------ admission gate

#[derive(Default)]
struct GateState {
    active: u32,
    active_background: u32,
    waiting_interactive: u32,
    waiting_background: u32,
    closed: bool,
}

impl GateState {
    fn waiting(&mut self, priority: Priority) -> &mut u32 {
        match priority {
            Priority::Interactive => &mut self.waiting_interactive,
            Priority::Background => &mut self.waiting_background,
        }
    }

    fn can_run(&self, slots: u32, priority: Priority) -> bool {
        if self.active >= slots {
            return false;
        }
        if priority == Priority::Interactive {
            return true;
        }
        if self.waiting_interactive > 0 {
            return false;
        }
        let background_cap = if slots > 1 { slots - 1 } else { 1 };
        self.active_background < background_cap
    }
}

/// Per-engine admission gate. At most `slots` requests run at once. Interactive requests always
/// go first; background requests never take the last slot when there is more than one, so a chat
/// turn is never behind a whole batch.
pub struct EngineGate {
    slots: u32,
    state: Mutex<GateState>,
    changed: Notify,
}

impl EngineGate {
    pub fn new(slots: u32) -> EngineGate {
        EngineGate {
            slots: slots.max(1),
            state: Mutex::new(GateState::default()),
            changed: Notify::new(),
        }
    }

    /// Waits for a slot. Fails when the gate is closed (the model was unloaded) meanwhile.
    pub async fn acquire(&self, priority: Priority) -> std::result::Result<(), AdmissionError> {
        *self.state.lock().waiting(priority) += 1;
        // Counts the request out of the queue if the caller gives up waiting.
        struct Waiting<'a> {
            gate: &'a EngineGate,
            priority: Priority,
            armed: bool,
        }
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                if self.armed {
                    let mut s = self.gate.state.lock();
                    let w = s.waiting(self.priority);
                    *w = w.saturating_sub(1);
                    drop(s);
                    self.gate.changed.notify_waiters();
                }
            }
        }
        let mut waiting = Waiting {
            gate: self,
            priority,
            armed: true,
        };
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut s = self.state.lock();
                if s.closed {
                    let w = s.waiting(priority);
                    *w = w.saturating_sub(1);
                    waiting.armed = false;
                    return Err(AdmissionError(
                        "The model was unloaded while the request was waiting.".into(),
                    ));
                }
                if s.can_run(self.slots, priority) {
                    let w = s.waiting(priority);
                    *w = w.saturating_sub(1);
                    waiting.armed = false;
                    s.active += 1;
                    if priority == Priority::Background {
                        s.active_background += 1;
                    }
                    // A background request held back only by this one may go now.
                    let wake = priority == Priority::Interactive
                        && s.waiting_interactive == 0
                        && s.waiting_background > 0;
                    drop(s);
                    if wake {
                        self.changed.notify_waiters();
                    }
                    return Ok(());
                }
            }
            notified.await;
        }
    }

    pub fn release(&self, priority: Priority) {
        {
            let mut s = self.state.lock();
            s.active = s.active.saturating_sub(1);
            if priority == Priority::Background {
                s.active_background = s.active_background.saturating_sub(1);
            }
        }
        self.changed.notify_waiters();
    }

    pub fn close(&self) {
        self.state.lock().closed = true;
        self.changed.notify_waiters();
    }

    pub fn slots(&self) -> u32 {
        self.slots
    }
    pub fn active(&self) -> u32 {
        self.state.lock().active
    }
    pub fn waiting_interactive(&self) -> u32 {
        self.state.lock().waiting_interactive
    }
    pub fn waiting_background(&self) -> u32 {
        self.state.lock().waiting_background
    }
}

// ------------------------------------------------------------------ lease

/// Handle for one in-flight request against a loaded engine (`RuntimeManager.Lease`). The model
/// stays busy until it is dropped (or [`Lease::close`]d).
pub struct Lease {
    engine: Arc<EngineProcess>,
    gate: Option<Arc<EngineGate>>,
    priority: Priority,
    closed: AtomicBool,
}

impl Lease {
    fn new(engine: Arc<EngineProcess>, gate: Option<Arc<EngineGate>>, priority: Priority) -> Lease {
        engine.begin_request();
        Lease {
            engine,
            gate,
            priority,
            closed: AtomicBool::new(false),
        }
    }

    pub fn client(&self) -> &InferenceClient {
        self.engine.client()
    }
    pub fn engine(&self) -> &Arc<EngineProcess> {
        &self.engine
    }
    pub fn priority(&self) -> Priority {
        self.priority
    }

    /// Ends the request; dropping the lease does the same.
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.engine.end_request();
            if let Some(gate) = &self.gate {
                gate.release(self.priority);
            }
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.close();
    }
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("model_id", &self.engine.model_id())
            .field("priority", &self.priority)
            .finish()
    }
}

impl api::Lease for Lease {
    fn client(&self) -> &InferenceClient {
        self.engine.client()
    }
}

/// Sets a busy flag for as long as it lives.
struct BusyFlag<'a>(&'a AtomicBool);

impl<'a> BusyFlag<'a> {
    fn set(flag: &'a AtomicBool) -> BusyFlag<'a> {
        flag.store(true, Ordering::SeqCst);
        BusyFlag(flag)
    }
}

impl Drop for BusyFlag<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

// ------------------------------------------------------------------ the manager

/// The runtime manager. Create it with [`RuntimeManager::new`] and share the `Arc`.
pub struct RuntimeManager {
    config: RuntimeConfig,
    options: RuntimeOptions,
    probe: SpeedProbe,
    engines: RwLock<HashMap<String, Arc<EngineProcess>>>,
    gates: RwLock<HashMap<String, Arc<EngineGate>>>,
    speech_engines: RwLock<HashMap<String, Arc<WhisperProcess>>>,
    pinned: RwLock<HashSet<String>>,
    /// Models the engine refused for a reason a restart cannot fix, by id (see
    /// [`ModelLoadError`]).
    refused: Mutex<HashMap<String, Refusal>>,
    recent_events: Mutex<VecDeque<RuntimeEvent>>,
    placement: tokio::sync::Mutex<()>,
    image_lock: tokio::sync::Mutex<()>,
    downloads: Mutex<BTreeMap<String, f64>>,
    download_workers: Arc<Semaphore>,
    hub_cancels: Mutex<HashMap<String, CancellationToken>>,
    last_downloads_event: Mutex<Option<Instant>>,
    backend: RwLock<Option<Backend>>,
    image_busy: AtomicBool,
    video_busy: AtomicBool,
    started: AtomicBool,
    /// Cancelled at shutdown: stops the sweep, the resume timer and downloads.
    stopping: CancellationToken,
    me: Weak<RuntimeManager>,
}

impl RuntimeManager {
    /// Builds the manager on the runtime's services with the original's timings. Starts no
    /// engine and touches no network; see [`RuntimeManager::start`].
    pub fn new(config: RuntimeConfig) -> Arc<RuntimeManager> {
        RuntimeManager::with_options(config, RuntimeOptions::default())
    }

    pub fn with_options(config: RuntimeConfig, options: RuntimeOptions) -> Arc<RuntimeManager> {
        // Without nvidia-smi the card's memory is read through the Vulkan engine's own device
        // list, once it is installed.
        let packages = config.packages.clone();
        config.inventory.set_vulkan_engine(Some(Arc::new(move || {
            packages
                .is_installed(EngineComponent::Llama, Backend::Vulkan)
                .then(|| packages.server_executable(Backend::Vulkan))
        })));
        if let Err(e) = config.home.ensure_layout() {
            tracing::warn!("Could not create runtime directories: {e:#}");
        }
        let probe = SpeedProbe::new(&config.home.runtime_dir());
        Arc::new_cyclic(|me| RuntimeManager {
            config,
            options,
            probe,
            engines: RwLock::new(HashMap::new()),
            gates: RwLock::new(HashMap::new()),
            speech_engines: RwLock::new(HashMap::new()),
            pinned: RwLock::new(HashSet::new()),
            refused: Mutex::new(HashMap::new()),
            recent_events: Mutex::new(VecDeque::new()),
            placement: tokio::sync::Mutex::new(()),
            image_lock: tokio::sync::Mutex::new(()),
            downloads: Mutex::new(BTreeMap::new()),
            download_workers: Arc::new(Semaphore::new(DOWNLOAD_WORKERS)),
            hub_cancels: Mutex::new(HashMap::new()),
            last_downloads_event: Mutex::new(None),
            backend: RwLock::new(None),
            image_busy: AtomicBool::new(false),
            video_busy: AtomicBool::new(false),
            started: AtomicBool::new(false),
            stopping: CancellationToken::new(),
            me: me.clone(),
        })
    }

    /// Background work after the window is up: selects the backend, sweeps for crashed and idle
    /// engines every five seconds, and picks up interrupted downloads after twenty. Safe to call
    /// more than once.
    pub async fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let backend = self.backend().await;
        tracing::info!("Runtime backend: {}", backend.label());
        let weak = Arc::downgrade(self);
        let stopping = self.stopping.clone();
        let every = self.options.sweep_every;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stopping.cancelled() => break,
                    _ = tokio::time::sleep(every) => {}
                }
                match weak.upgrade() {
                    Some(me) => me.sweep().await,
                    None => break,
                }
            }
        });
        let weak = Arc::downgrade(self);
        let stopping = self.stopping.clone();
        let after = self.options.resume_after;
        tokio::spawn(async move {
            tokio::select! {
                _ = stopping.cancelled() => {}
                _ = tokio::time::sleep(after) => {
                    if let Some(me) = weak.upgrade() {
                        me.resume_interrupted_downloads();
                    }
                }
            }
        });
    }

    /// A download that was cut off by closing the app picks up where it left off on the next
    /// start, so a person never has to notice. Anything with a `.part` beside a catalog artifact
    /// counts.
    pub fn resume_interrupted_downloads(&self) {
        let registry = &self.config.registry;
        for m in self.config.catalog.all() {
            if registry.is_installed(&m.id) || self.downloads.lock().contains_key(&m.id) {
                continue;
            }
            let partial = m
                .artifacts
                .iter()
                .any(|a| part_path(&registry.artifact_target(m, &a.file)).exists());
            if partial {
                self.emit(
                    "download_resumed",
                    Some(&m.id),
                    "picking up an interrupted download".to_string(),
                );
                if let Err(e) = self.download_async(&m.id) {
                    tracing::warn!("Could not resume downloads: {e:#}");
                }
            }
        }
    }

    // ------------------------------------------------------------------ wiring

    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }
    pub fn inventory(&self) -> &Arc<GpuInventory> {
        &self.config.inventory
    }
    pub fn packages(&self) -> &Arc<EnginePackages> {
        &self.config.packages
    }
    pub fn registry(&self) -> &Arc<ModelRegistry> {
        &self.config.registry
    }
    pub fn catalog(&self) -> &Arc<ModelCatalog> {
        &self.config.catalog
    }
    pub fn hub(&self) -> &Arc<HuggingFaceHub> {
        &self.config.hub
    }
    pub fn probe(&self) -> &SpeedProbe {
        &self.probe
    }

    /// The engine build the runtime runs on, chosen once (an override, else CUDA when an NVIDIA
    /// card answers, else Vulkan).
    pub async fn backend(&self) -> Backend {
        if let Some(b) = *self.backend.read() {
            return b;
        }
        let b = match self.options.backend {
            Some(b) => b,
            None => self.config.inventory.select_backend().await,
        };
        *self.backend.write() = Some(b);
        b
    }

    /// The backend without waiting: the chosen one, or before [`start`](Self::start) has chosen,
    /// the override or what the inventory has seen so far (Vulkan when nothing yet).
    pub fn backend_now(&self) -> Backend {
        if let Some(b) = *self.backend.read() {
            return b;
        }
        if let Some(b) = self.options.backend {
            return b;
        }
        if let Ok(v) = std::env::var(BACKEND_ENV) {
            if let Ok(b) = Backend::from_id(v.trim()) {
                return b;
            }
        }
        if self.config.inventory.has_nvidia() {
            Backend::Cuda
        } else {
            Backend::Vulkan
        }
    }

    /// Publishes an event to the feed and the UI.
    pub fn notify(&self, kind: &str, model_id: Option<&str>, detail: Option<String>) {
        self.emit(kind, model_id, detail);
    }

    /// The most recent events, newest first, for the Settings runtime page.
    pub fn recent_events(&self) -> Vec<RuntimeEvent> {
        self.recent_events.lock().iter().cloned().collect()
    }

    fn emit(&self, kind: &str, model_id: Option<&str>, detail: impl Into<Option<String>>) {
        let e = RuntimeEvent {
            kind: kind.to_string(),
            model_id: model_id.map(str::to_string),
            detail: detail.into(),
            at: Utc::now(),
        };
        {
            let mut recent = self.recent_events.lock();
            recent.push_front(e.clone());
            recent.truncate(RECENT_EVENTS_MAX);
        }
        tracing::info!(
            "runtime event {kind} {} {}",
            model_id.unwrap_or(""),
            e.detail.as_deref().unwrap_or("")
        );
        events::emit(topic::RUNTIME, &e);
    }

    fn executable(&self, component: EngineComponent, backend: Backend) -> PathBuf {
        if let Some(locate) = &self.options.executables {
            return locate(component, backend);
        }
        let packages = &self.config.packages;
        match component {
            EngineComponent::Llama => packages.server_executable(backend),
            EngineComponent::Whisper => {
                packages.executable(component, backend, &["whisper-server.exe", "server.exe"])
            }
            EngineComponent::Sd => {
                packages.executable(component, backend, &["sd-cli.exe", "sd.exe"])
            }
            EngineComponent::Ffmpeg => {
                packages.executable(component, backend, &["bin/ffmpeg.exe", "ffmpeg.exe"])
            }
            EngineComponent::Audio => {
                packages.executable(component, backend, &["audiocpp_cli.exe"])
            }
            EngineComponent::Pdfium => {
                packages.executable(component, backend, &["bin/pdfium.dll", "pdfium.dll"])
            }
            EngineComponent::Pandoc => packages.executable(component, backend, &["pandoc.exe"]),
            EngineComponent::Office => packages.executable(
                component,
                backend,
                &[
                    "program/soffice.com",
                    "LibreOffice/program/soffice.com",
                    "PFiles/LibreOffice/program/soffice.com",
                ],
            ),
        }
    }

    fn system_ram(&self) -> u64 {
        self.options
            .system_ram_bytes
            .unwrap_or_else(gpu_inventory::physical_ram_bytes)
    }

    // ------------------------------------------------------------------ engine packages

    pub fn is_engine_installed(&self) -> bool {
        self.is_component_installed(EngineComponent::Llama)
    }

    pub fn is_component_installed(&self, component: EngineComponent) -> bool {
        self.config
            .packages
            .is_installed(component, self.backend_now())
    }

    /// Downloads and installs the text engine for the selected backend.
    pub async fn ensure_engine_installed(
        &self,
        progress: Option<StagedProgress>,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        self.ensure_component(EngineComponent::Llama, progress, cancel)
            .await
    }

    pub async fn ensure_component(
        &self,
        component: EngineComponent,
        progress: Option<StagedProgress>,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let b = self.backend().await;
        let packages = &self.config.packages;
        let ok = packages
            .ensure_installed(component, b, progress, cancel)
            .await?;
        if ok {
            self.emit(
                "engine_installed",
                None,
                format!(
                    "{} {} ({})",
                    component.id(),
                    packages.version_of(component),
                    b.id()
                ),
            );
        }
        Ok(ok)
    }

    pub fn readiness(&self) -> Readiness {
        if !self.is_engine_installed() {
            return Readiness::EngineMissing;
        }
        if !self.config.registry.list().iter().any(LocalModel::is_chat) {
            return Readiness::NoModels;
        }
        Readiness::Ready
    }

    /// HubScreen's first-start check: models are installed (a backend switch, a deleted runtime)
    /// but the text engine is not.
    pub async fn needs_engine(&self) -> bool {
        let b = self.backend().await;
        !self.config.packages.is_installed(EngineComponent::Llama, b)
            && !self.config.registry.list().is_empty()
    }

    /// Brings the text engine back in the background of the first screen and returns the note
    /// HubScreen showed.
    pub async fn install_missing_engine(&self) -> String {
        let label = self.backend().await.label();
        match self
            .ensure_engine_installed(None, &self.stopping.child_token())
            .await
        {
            Ok(true) => format!("The Nook runtime ({label}) is installed."),
            Ok(false) => "The Nook runtime install stopped.".to_string(),
            Err(e) => format!("Runtime install failed: {e}"),
        }
    }

    // ------------------------------------------------------------------ downloads

    /// Progress (0..1) of in-flight catalog and Hub downloads started through
    /// [`download_async`](Self::download_async) and [`download_hub_async`](Self::download_hub_async).
    pub fn downloads(&self) -> BTreeMap<String, f64> {
        self.downloads.lock().clone()
    }

    fn emit_downloads(&self, force: bool) {
        {
            let mut last = self.last_downloads_event.lock();
            if !force && last.is_some_and(|t| t.elapsed() < PROGRESS_UI_EVERY) {
                return;
            }
            *last = Some(Instant::now());
        }
        events::emit(
            topic::DOWNLOADS,
            RuntimeDownloads {
                source: "runtime".into(),
                downloads: self.downloads(),
            },
        );
    }

    /// Downloads a catalog model, installing the engine component it needs first. The component
    /// download is folded into the same progress scale so the UI shows one bar.
    ///
    /// Returns true when installed, false when cancelled.
    pub async fn download(
        &self,
        model_id: &str,
        progress: Option<Progress>,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let model = self
            .config
            .catalog
            .find(model_id)
            .ok_or_else(|| anyhow!("Unknown catalog model {model_id}"))?
            .clone();
        let component = model.component();
        let b = self.backend().await;
        let packages = &self.config.packages;
        let model_bytes = model.total_bytes();
        let engine_bytes = if packages.is_installed(component, b) {
            0
        } else {
            packages.package_for(component, b)?.total_bytes()
        };
        let total = model_bytes + engine_bytes;
        if engine_bytes > 0 {
            self.emit(
                "engine_installing",
                Some(model_id),
                component.id().to_string(),
            );
            let p = progress.clone();
            let staged: StagedProgress = Arc::new(move |_stage, done, _| {
                if let Some(p) = &p {
                    p(done.min(engine_bytes), total);
                }
            });
            if !packages
                .ensure_installed(component, b, Some(staged), cancel)
                .await?
            {
                return Ok(false);
            }
            self.emit(
                "engine_installed",
                None,
                format!("{} {}", component.id(), packages.version_of(component)),
            );
        }
        let p = progress.clone();
        let model_progress: Progress = Arc::new(move |done, _| {
            if let Some(p) = &p {
                p(engine_bytes + done, total);
            }
        });
        self.config
            .registry
            .download(model_id, Some(model_progress), cancel)
            .await
    }

    /// A progress callback that keeps the downloads map, a `download_progress` event every two
    /// seconds and the UI's downloads topic current.
    fn download_reporter(self: &Arc<Self>, key: &str) -> Progress {
        let me = Arc::downgrade(self);
        let key = key.to_string();
        let last_event: Mutex<Option<Instant>> = Mutex::new(None);
        Arc::new(move |done, total| {
            let Some(me) = me.upgrade() else { return };
            let p = if total > 0 {
                done as f64 / total as f64
            } else {
                0.0
            };
            me.downloads.lock().insert(key.clone(), p);
            me.emit_downloads(false);
            let due = {
                let mut last = last_event.lock();
                let due = last.is_none_or(|t| t.elapsed() > PROGRESS_EVENT_EVERY);
                if due {
                    *last = Some(Instant::now());
                }
                due
            };
            if due {
                me.emit(
                    "download_progress",
                    Some(&key),
                    format!("{:.1}% ({}/{} MB)", p * 100.0, done >> 20, total >> 20),
                );
            }
        })
    }

    fn spawn_download(
        self: &Arc<Self>,
        work: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow!("Downloads need the app's async runtime"))?;
        let workers = self.download_workers.clone();
        handle.spawn(async move {
            let Ok(_permit) = workers.acquire_owned().await else {
                return;
            };
            work.await;
        });
        Ok(())
    }

    /// Starts a catalog download in the background. Progress is published as `download_progress`
    /// events and in [`downloads`](Self::downloads); completion as `model_downloaded`,
    /// `download_cancelled` or `download_failed`. False when that model is already downloading.
    pub fn download_async(&self, model_id: &str) -> Result<bool> {
        if self.config.catalog.find(model_id).is_none() {
            bail!("Unknown catalog model {model_id}");
        }
        let Some(me) = self.me.upgrade() else {
            bail!("The runtime is shutting down");
        };
        {
            let mut downloads = self.downloads.lock();
            if downloads.contains_key(model_id) {
                return Ok(false);
            }
            downloads.insert(model_id.to_string(), 0.0);
        }
        self.emit_downloads(true);
        let id = model_id.to_string();
        let worker = me.clone();
        let cancel = self.stopping.child_token();
        let spawned = me.spawn_download(async move {
            let me = worker;
            let report = me.download_reporter(&id);
            match me.download(&id, Some(report), &cancel).await {
                Ok(ok) => me.emit(
                    if ok {
                        "model_downloaded"
                    } else {
                        "download_cancelled"
                    },
                    Some(&id),
                    None,
                ),
                Err(e) => me.emit("download_failed", Some(&id), format!("{e:#}")),
            }
            me.downloads.lock().remove(&id);
            me.emit_downloads(true);
        });
        if let Err(e) = spawned {
            self.downloads.lock().remove(model_id);
            return Err(e);
        }
        Ok(true)
    }

    /// The key under which a Hub download reports progress in [`downloads`](Self::downloads).
    pub fn hub_key(repo_id: &str, variant_key: &str) -> String {
        format!("hub:{repo_id}:{variant_key}")
    }

    /// Downloads a Hugging Face variant in the background, installing the text engine first when
    /// it is missing. Progress and completion use the same events and map as catalog downloads,
    /// so the model list and the settings page both pick it up. False when it is already
    /// downloading.
    pub fn download_hub_async(&self, repo: Repo, variant: Variant) -> bool {
        let key = RuntimeManager::hub_key(&repo.id, &variant.key);
        let Some(me) = self.me.upgrade() else {
            return false;
        };
        {
            let mut downloads = self.downloads.lock();
            if downloads.contains_key(&key) {
                return false;
            }
            downloads.insert(key.clone(), 0.0);
        }
        let cancel = self.stopping.child_token();
        self.hub_cancels.lock().insert(key.clone(), cancel.clone());
        self.emit_downloads(true);
        let worker = me.clone();
        let k = key.clone();
        let spawned = me.spawn_download(async move {
            let me = worker;
            let key = k;
            if let Err(e) = me.download_hub(&repo, &variant, &key, &cancel).await {
                me.emit("download_failed", Some(&key), format!("{e:#}"));
            }
            me.downloads.lock().remove(&key);
            me.hub_cancels.lock().remove(&key);
            me.emit_downloads(true);
        });
        if let Err(e) = spawned {
            tracing::warn!("Could not start the Hub download {key}: {e:#}");
            self.downloads.lock().remove(&key);
            self.hub_cancels.lock().remove(&key);
            return false;
        }
        true
    }

    async fn download_hub(
        self: &Arc<Self>,
        repo: &Repo,
        variant: &Variant,
        key: &str,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let component = EngineComponent::for_task("chat");
        let b = self.backend().await;
        let packages = &self.config.packages;
        let engine_bytes = if packages.is_installed(component, b) {
            0
        } else {
            packages.package_for(component, b)?.total_bytes()
        };
        let total = variant.total_bytes + engine_bytes;
        let report = self.download_reporter(key);
        if engine_bytes > 0 {
            self.emit("engine_installing", Some(key), component.id().to_string());
            let r = report.clone();
            let staged: StagedProgress =
                Arc::new(move |_stage, done, _| r(done.min(engine_bytes), total));
            if !packages
                .ensure_installed(component, b, Some(staged), cancel)
                .await?
            {
                self.emit("download_cancelled", Some(key), None);
                return Ok(());
            }
            self.emit(
                "engine_installed",
                None,
                format!("{} {}", component.id(), packages.version_of(component)),
            );
        }
        let r = report.clone();
        let progress: Progress = Arc::new(move |done, _| r(engine_bytes + done, total));
        let ok = self
            .config
            .hub
            .download(repo, variant, Some(progress), cancel)
            .await?;
        self.config.registry.invalidate();
        if ok {
            self.emit(
                "model_downloaded",
                Some(key),
                hugging_face_hub::model_id(repo, variant),
            );
        } else {
            self.emit("download_cancelled", Some(key), None);
        }
        Ok(())
    }

    /// Stops a Hub download; the partial file stays so the next attempt resumes.
    pub fn cancel_hub_download(&self, repo_id: &str, variant_key: &str) {
        if let Some(c) = self
            .hub_cancels
            .lock()
            .get(&RuntimeManager::hub_key(repo_id, variant_key))
        {
            c.cancel();
        }
    }

    // ------------------------------------------------------------------ text admission

    /// Returns a lease on a loaded engine for the model, loading it first if needed; waits while
    /// the model loads. Interactive requests skip ahead of queued background requests and may
    /// use every slot; background requests leave one slot free when the engine has more than one.
    /// Fails with a user-readable message when the model cannot be placed.
    pub async fn acquire(&self, model_id: &str, priority: Priority) -> Result<Lease> {
        let current = self.engine(model_id);
        let e = match current {
            Some(e) if e.state() == State::Ready && e.is_alive() => e,
            _ => self.load(model_id).await?,
        };
        let gate = self
            .gates
            .write()
            .entry(model_id.to_string())
            .or_insert_with(|| Arc::new(EngineGate::new(1)))
            .clone();
        gate.acquire(priority).await?;
        let now = self.engine(model_id);
        let same = now.as_ref().is_some_and(|c| Arc::ptr_eq(c, &e));
        if !same || !e.is_alive() {
            gate.release(priority);
            return Err(AdmissionError(
                "The model was reloaded while the request was waiting; please retry.".into(),
            )
            .into());
        }
        Ok(Lease::new(e, Some(gate), priority))
    }

    /// Loads the model (nothing to do when already resident) and returns its engine.
    pub async fn load(&self, model_id: &str) -> Result<Arc<EngineProcess>> {
        let _placement = self.placement.lock().await;
        let existing = self.engine(model_id);
        if let Some(e) = &existing {
            if e.state() == State::Ready && e.is_alive() {
                return Ok(e.clone());
            }
        }
        if let Some(e) = existing {
            e.stop().await;
            self.engines.write().remove(model_id);
        }
        let b = self.backend().await;
        if !self.config.packages.is_installed(EngineComponent::Llama, b) {
            bail!("The Nook runtime is not installed yet.");
        }
        let model = self
            .config
            .registry
            .find(model_id)
            .ok_or_else(|| anyhow!("Model '{model_id}' is not downloaded."))?;
        // A file whose header is no language model's (a vision encoder from the Hub) is not
        // handed to the engine at all.
        if let Some(message) = model.unsupported_message() {
            self.emit("model_failed", Some(model_id), message.clone());
            return Err(ModelLoadError {
                message,
                permanent: true,
            }
            .into());
        }
        let Some(meta) = model.metadata.clone() else {
            bail!(
                "Model '{model_id}' is a {} model, not a text model.",
                model.task
            );
        };
        let exe = self.executable(EngineComponent::Llama, b);
        // The engine said no to this very file before, for a reason a restart cannot fix: say
        // so again at once rather than start it again (six starts in fifteen seconds for one
        // Code request on 2026-09-26).
        let stamp = [file_stamp(&model.file), file_stamp(&exe)];
        let refused = {
            let mut refused = self.refused.lock();
            match refused.get(model_id) {
                Some(r) if r.stamp == stamp => Some(r.error.clone()),
                _ => {
                    refused.remove(model_id);
                    None
                }
            }
        };
        if let Some(error) = refused {
            tracing::info!("Not starting the engine for {model_id} again: {error}");
            return Err(error.into());
        }

        let mut plan = self.plan(&model).await;
        if meta.is_embedding_model() {
            plan = plan.as_embedding();
        }
        let engine = Arc::new(EngineProcess::new(
            model_id,
            &model.file,
            b,
            &exe,
            &self.config.home.engine_logs_dir(),
            plan.clone(),
        )?);
        self.emit(
            "model_loading",
            Some(model_id),
            format!(
                "ngl={} ctx={} per request x {} slots (--ctx-size {}){}",
                plan.gpu_layers,
                plan.ctx_per_slot,
                plan.slots,
                plan.ctx_total(),
                plan.tensor_split
                    .as_ref()
                    .map(|s| format!(" split={s}"))
                    .unwrap_or_default()
            ),
        );
        self.engines
            .write()
            .insert(model_id.to_string(), engine.clone());
        if let Err(e) = engine.start(self.options.start_timeout).await {
            self.engines.write().remove(model_id);
            // What the engine's log says about why it stopped, in words: the log's tail says
            // it to whoever reads the log, not to the person who asked.
            let cause = e
                .downcast_ref::<EngineExited>()
                .and_then(|x| x.cause.clone());
            let Some(cause) = cause else {
                self.emit("model_failed", Some(model_id), e.to_string());
                return Err(e);
            };
            tracing::warn!("The engine could not load {model_id}: {e}");
            let engine_name = format!("llama.cpp {}", self.config.packages.version());
            let error = ModelLoadError::from_engine(&model.display_name, &engine_name, &cause);
            self.emit("model_failed", Some(model_id), error.message.clone());
            if error.permanent {
                self.refused.lock().insert(
                    model_id.to_string(),
                    Refusal {
                        stamp,
                        error: error.clone(),
                    },
                );
            }
            return Err(error.into());
        }
        let old = self
            .gates
            .write()
            .insert(model_id.to_string(), Arc::new(EngineGate::new(plan.slots)));
        if let Some(old) = old {
            old.close();
        }
        let confirms = if engine.engine_ctx_per_slot() > 0 {
            format!(
                " (engine confirms {} x {})",
                engine.engine_ctx_per_slot(),
                engine.engine_slots()
            )
        } else {
            String::new()
        };
        self.emit(
            "model_loaded",
            Some(model_id),
            format!(
                "port={} slots={} ctx={} per request{confirms}",
                engine.port(),
                plan.slots,
                plan.ctx_per_slot
            ),
        );
        if let Some(mismatch) = engine.context_mismatch() {
            tracing::warn!("Context units for {model_id}: {mismatch}");
            self.emit("context_mismatch", Some(model_id), mismatch);
        }
        if !meta.is_embedding_model() {
            self.schedule_probe(&model, &plan);
        }
        Ok(engine)
    }

    /// True when nothing is using the GPU through Nook: no request in flight or waiting on any
    /// engine, no transcription, no image, no video.
    pub fn idle(&self) -> bool {
        if self.image_busy.load(Ordering::SeqCst) || self.video_busy.load(Ordering::SeqCst) {
            return false;
        }
        let gates = self.gates.read();
        for e in self.engines.read().values() {
            if e.in_flight() > 0 {
                return false;
            }
            if let Some(g) = gates.get(e.model_id()) {
                if g.active() > 0 || g.waiting_interactive() > 0 || g.waiting_background() > 0 {
                    return false;
                }
            }
        }
        !self
            .speech_engines
            .read()
            .values()
            .any(|w| w.in_flight() > 0)
    }

    /// How hard Nook is working the GPU, 0 to 1, for the mark in the title strip, which spins at
    /// this share of its top speed. 0 when [`idle`](Self::idle) and no model is loading.
    /// Otherwise the busiest card's utilisation where the driver reports one (nvidia-smi), or
    /// else the share of the loaded engines' slots in use (AMD and Intel through Vulkan report
    /// memory only). The card's reading covers the whole machine, so it is asked only while Nook
    /// is at work: a browser or a game on its own never turns the mark.
    pub async fn gpu_load(&self) -> f64 {
        let loading = self
            .engines
            .read()
            .values()
            .any(|e| e.state() == State::Starting);
        if self.idle() && !loading {
            return 0.0;
        }
        let (mut active, mut slots) = (0, 0);
        {
            let gates = self.gates.read();
            for e in self.engines.read().values() {
                if let Some(g) = gates.get(e.model_id()) {
                    active += g.active();
                    slots += g.slots();
                }
            }
        }
        busy_load(self.config.inventory.utilization().await, active, slots)
    }

    /// The GPU driver the probe's numbers belong to; a new driver re-measures. `cpu` without a
    /// GPU; None when the device does not report its driver (the Vulkan engine's list).
    pub async fn driver_version(&self) -> Option<String> {
        let snapshot = self.config.inventory.snapshot().await;
        match snapshot.devices.first() {
            None => Some("cpu".to_string()),
            Some(d) => d.driver_version.clone(),
        }
    }

    /// The speed probe, once per model pin and driver: queued as background work behind whatever
    /// loaded the model, so the person's own request goes first. About thirty seconds of the card.
    /// (Whether the stored result still holds is checked in the queued work, which keeps this
    /// free of waiting: the probe's own `acquire` may load the model again.)
    fn schedule_probe(&self, model: &LocalModel, plan: &Plan) {
        let Some(me) = self.me.upgrade() else { return };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let pin = pin_of(model);
        let id = model.id.clone();
        let gpu_layers = plan.gpu_layers;
        let delay = self.options.probe_delay;
        let stopping = self.stopping.clone();
        handle.spawn(async move {
            let driver = me.driver_version().await;
            if me
                .probe
                .current(&id, Some(&pin), driver.as_deref())
                .is_some()
            {
                return;
            }
            tokio::select! {
                _ = stopping.cancelled() => return,
                _ = tokio::time::sleep(delay) => {}
            }
            let result = async {
                let lease = me.acquire(&id, Priority::Background).await?;
                let backend = me.backend().await;
                me.probe
                    .measure(
                        lease.client(),
                        &id,
                        Some(&pin),
                        driver.as_deref(),
                        backend.id(),
                        gpu_layers,
                    )
                    .await
            }
            .await;
            match result {
                Ok(r) => me.emit(
                    "probe_measured",
                    Some(&id),
                    format!(
                        "generate {} tok/s, prompt {} tok/s{}",
                        super::engine_process::java_double(r.generate_tps),
                        super::engine_process::java_double(r.prompt_tps),
                        if r.fast_enough_to_work() {
                            String::new()
                        } else {
                            format!(" (under {}: too slow for Nook Code)", MIN_WORKER_TPS as i64)
                        }
                    ),
                ),
                Err(e) => tracing::debug!("Speed probe for {id} skipped: {e:#}"),
            }
        });
    }

    /// How many parallel slots a fully loaded model gets: two when a second cache fits in what is
    /// spare, four when three more do; workers.json `"slots"` (a QA knob, 2026-09-22, P4)
    /// overrides that when the extra caches fit, else the computed count stands.
    fn slots_for(&self, fp: &Footprint, budget: u64, model: &LocalModel) -> u32 {
        let spare = budget as i64 - fp.total_bytes() as i64;
        let kv = fp.kv_bytes as i64;
        let mut slots = 1;
        if kv > 0 && spare > 3 * kv {
            slots = 4;
        } else if kv > 0 && spare > kv {
            slots = 2;
        }
        let wanted = self.preference("slots", 1, 8) as u32;
        if wanted > 0 && wanted != slots {
            if wanted == 1 || kv == 0 || spare > (wanted as i64 - 1) * kv {
                self.emit(
                    "slots_override",
                    Some(&model.id),
                    format!(
                        "{wanted} slot{} from workers.json (the plan would run {slots})",
                        if wanted == 1 { "" } else { "s" }
                    ),
                );
                slots = wanted;
            } else {
                self.emit(
                    "slots_override",
                    Some(&model.id),
                    format!(
                        "workers.json asks for {wanted} slots but only {slots} fit ({} MB spare, {} MB per slot's cache)",
                        spare >> 20,
                        kv >> 20
                    ),
                );
            }
        }
        slots
    }

    /// A numeric entry of workers.json within `[min, max]`, else 0: `"slots"` and `"ctx"`
    /// (context per request) let a person or a QA run pick a placement the planner would not,
    /// within what fits.
    fn preference(&self, key: &str, min: i64, max: i64) -> i64 {
        self.config
            .registry
            .worker_preferences()
            .get(key)
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|n| *n >= min && *n <= max)
            .unwrap_or(0)
    }

    /// Placement: fit the whole model on the GPU if possible, otherwise evict idle engines in LRU
    /// order, otherwise offload as many layers as fit. Called under the placement lock.
    async fn plan(&self, model: &LocalModel) -> Plan {
        let b = self.backend().await;
        let Some(meta) = model.metadata.as_deref() else {
            return Plan::new(0, 8192, 1, false, false);
        };
        let inventory = &self.config.inventory;
        // The context one request sees; the launcher multiplies by the slots for --ctx-size.
        let n_ctx = self.requested_ctx(model, true);
        let gpu = b != Backend::Cpu;
        let kv_quantized = b == Backend::Cuda && !meta.is_embedding_model();
        let flash = b == Backend::Cuda;

        if !gpu {
            return Plan::new(0, n_ctx, 1, false, false);
        }

        // Vulkan fits its own layers (--fit): the engine's view of an AMD or Intel card's memory
        // is the reliable one. Since 2026-09-22 the inventory reads that same view through the
        // engine's device list, so the experts-in-RAM decision, idle eviction and the slot count
        // work from a budget as they do on CUDA; without a reading (engine not installed yet) it
        // is one slot.
        if b == Backend::Vulkan {
            let mut budget = inventory.budget_bytes().await;
            if budget == 0 {
                return Plan::new(-1, n_ctx, 1, false, false);
            }
            let seen = inventory.last_seen();
            if let Some(first) = seen.first().filter(|d| d.integrated) {
                self.emit(
                    "integrated_graphics",
                    Some(&model.id),
                    format!(
                        "the only graphics device is {}, which shares the machine's memory; it will be slow, and the CPU may do as well",
                        first.name
                    ),
                );
            }
            let fp = Footprint::of(meta, n_ctx, 1, false);
            if !fp.fits_fully(budget) && meta.is_mixture_of_experts() {
                if let Some(plan) = self.experts_in_ram(model, &fp, budget, n_ctx, false, false) {
                    return plan;
                }
            }
            if !fp.fits_fully(budget) {
                budget = self.evict_idle(fp.total_bytes(), &model.id, false).await;
            }
            let slots = if fp.fits_fully(budget) {
                self.slots_for(&fp, budget, model)
            } else {
                1
            };
            return Plan::new(-1, n_ctx, slots, false, false).with_split(inventory.tensor_split());
        }

        let fp = Footprint::of(meta, n_ctx, 1, kv_quantized);
        let mut budget = inventory.budget_bytes().await;
        if !fp.fits_fully(budget) && meta.is_mixture_of_experts() {
            // A 30B-class MoE never fits an 8 GB card, but its 3B of active weights run well with
            // the experts in system RAM; paging layers or evicting a small model would not get it
            // there.
            if let Some(plan) = self.experts_in_ram(model, &fp, budget, n_ctx, kv_quantized, flash)
            {
                return plan;
            }
        }
        if !fp.fits_fully(budget) {
            budget = self.evict_idle(fp.total_bytes(), &model.id, false).await;
        }
        let layers = fp.gpu_layers_for(budget);
        let split = inventory.tensor_split();
        if layers > fp.layers {
            return Plan::new(
                999,
                n_ctx,
                self.slots_for(&fp, budget, model),
                kv_quantized,
                flash,
            )
            .with_split(split);
        }
        if layers == 0 {
            self.emit(
                "cpu_fallback",
                Some(&model.id),
                format!(
                    "no GPU memory available ({} MB free, model needs {} MB); running on the CPU. Close other GPU applications for speed.",
                    budget >> 20,
                    fp.total_bytes() >> 20
                ),
            );
            return Plan::new(0, n_ctx, 1, false, false).with_split(split);
        }
        // Tight memory: shorten the context and run two slots so a chat turn never queues behind
        // a background request. Costs about one extra layer of offload for the second KV cache.
        let tight_ctx = n_ctx.min(4096);
        let fp2 = Footprint::of(meta, tight_ctx, 2, kv_quantized);
        let layers2 = fp2.gpu_layers_for(budget);
        if layers2 as i64 >= layers as i64 - 2 && layers2 > 0 {
            self.emit(
                "partial_offload",
                Some(&model.id),
                format!(
                    "{layers2} of {} layers on GPU with 2 slots of {tight_ctx} context each (--ctx-size {}; {} MB free, full model needs {} MB). Close other GPU applications to load it fully.",
                    fp2.layers,
                    fp2.ctx_total(),
                    budget >> 20,
                    fp.total_bytes() >> 20
                ),
            );
            return Plan::new(layers2 as i32, tight_ctx, 2, kv_quantized, flash).with_split(split);
        }
        self.emit(
            "partial_offload",
            Some(&model.id),
            format!(
                "{layers} of {} layers on GPU ({} MB free, model needs {} MB). Close other GPU applications to load it fully.",
                fp.layers,
                budget >> 20,
                fp.total_bytes() >> 20
            ),
        );
        Plan::new(layers as i32, n_ctx, 1, kv_quantized, flash).with_split(split)
    }

    /// The experts-in-RAM plan for a mixture-of-experts model that does not fit, or None (with a
    /// `partial_offload` event) when the machine lacks the memory for it.
    fn experts_in_ram(
        &self,
        model: &LocalModel,
        fp: &Footprint,
        budget: u64,
        n_ctx: u32,
        kv_quantized: bool,
        flash: bool,
    ) -> Option<Plan> {
        let meta = model.metadata.as_deref()?;
        let ram = self.system_ram();
        if ram > 0 && ram < MIN_RAM_FOR_EXPERTS {
            self.emit(
                "partial_offload",
                Some(&model.id),
                format!(
                    "a mixture-of-experts model needs about {} GB of system memory to keep its experts in RAM; this machine has {} GB",
                    MIN_RAM_FOR_EXPERTS >> 30,
                    ram >> 30
                ),
            );
            return None;
        }
        self.emit(
            "experts_in_ram",
            Some(&model.id),
            format!(
                "{} experts per layer in system memory, attention on the GPU ({} MB free, whole model {} MB)",
                meta.expert_count(),
                budget >> 20,
                fp.total_bytes() >> 20
            ),
        );
        Some(
            Plan::new(-1, n_ctx, 1, kv_quantized, flash)
                .with_split(self.config.inventory.tensor_split())
                .with_experts_in_ram(),
        )
    }

    /// The context one request to `model` is planned with: the catalog's, within what the model
    /// was trained for, or workers.json's `"ctx"` (`report`: said as a ctx_override event). A load
    /// under tight memory can still give less.
    fn requested_ctx(&self, model: &LocalModel, report: bool) -> u32 {
        let Some(meta) = model.metadata.as_deref() else {
            return 0;
        };
        let trained = meta.context_length().max(512);
        let catalog_ctx = self
            .config
            .catalog
            .find(&model.id)
            .map(|c| c.default_ctx())
            .unwrap_or(8192);
        let mut n_ctx = catalog_ctx.min(trained);
        let wanted = self.preference("ctx", 512, 1 << 20) as u32;
        if wanted > 0 && wanted != n_ctx && !meta.is_embedding_model() {
            let allowed = wanted.min(trained);
            if report {
                self.emit(
                    "ctx_override",
                    Some(&model.id),
                    format!(
                        "{allowed} tokens per request from workers.json (the catalog says {n_ctx})"
                    ),
                );
            }
            n_ctx = allowed;
        }
        n_ctx
    }

    /// The context one request to `model_id` has: while its engine runs, the engine's own
    /// number, which a load under tight memory makes smaller than planned; otherwise what a load
    /// would plan. 0 for a model that is not installed.
    pub fn context_per_request(&self, model_id: &str) -> u32 {
        if let Some(e) = self.engine(model_id) {
            return if e.engine_ctx_per_slot() > 0 {
                e.engine_ctx_per_slot() as u32
            } else {
                e.plan().ctx_per_slot
            };
        }
        self.config
            .registry
            .find(model_id)
            .filter(|m| m.metadata.is_some())
            .map(|m| self.requested_ctx(&m, false))
            .unwrap_or(0)
    }

    /// Evicts idle engines, least recently used first, until the budget covers `need_bytes`.
    /// Unpinned engines go first; pinned ones only when `include_pinned` is set. Returns the
    /// budget after eviction.
    async fn evict_idle(&self, need_bytes: u64, for_model: &str, include_pinned: bool) -> u64 {
        let inventory = &self.config.inventory;
        let mut budget = inventory.budget_bytes().await;
        let pinned = self.pinned.read().clone();
        let mut idle: Vec<Arc<EngineProcess>> = self
            .engines
            .read()
            .values()
            .filter(|e| e.in_flight() == 0 && (include_pinned || !pinned.contains(e.model_id())))
            .filter(|e| e.model_id() != for_model)
            .cloned()
            .collect();
        idle.sort_by_key(|e| (pinned.contains(e.model_id()), e.last_used()));
        let idle_speech: Vec<Arc<WhisperProcess>> = self
            .speech_engines
            .read()
            .values()
            .filter(|w| w.in_flight() == 0)
            .cloned()
            .collect();
        // Text engines first, then speech, as before; the eviction plan keeps small models
        // resident when evicting them would not make the incoming model fit anyway (QA finding 3).
        let mut estimates: Vec<u64> = idle
            .iter()
            .map(|e| self.resident_estimate(e.model_id(), 1))
            .collect();
        estimates.extend(
            idle_speech
                .iter()
                .map(|w| self.resident_estimate(w.model_id(), 2) + (512 << 20)),
        );
        for index in eviction_plan::victims(budget, need_bytes, &estimates) {
            if budget >= need_bytes {
                break;
            }
            let reason = format!("evicted for {for_model}");
            if index < idle.len() {
                self.unload_internal(idle[index].model_id(), &reason).await;
            } else {
                let w = &idle_speech[index - idle.len()];
                self.speech_engines.write().remove(w.model_id());
                w.stop().await;
                self.emit("model_evicted", Some(w.model_id()), reason);
            }
            budget = inventory.budget_bytes().await;
        }
        budget
    }

    /// An upper bound on what a resident model holds: its file size times a factor, or "large"
    /// when unknown.
    fn resident_estimate(&self, model_id: &str, factor: u64) -> u64 {
        self.config
            .registry
            .find(model_id)
            .map(|m| m.bytes * factor)
            .unwrap_or(64 << 30)
    }

    /// Unloads a text or speech model.
    pub async fn unload(&self, model_id: &str) {
        let _placement = self.placement.lock().await;
        self.unload_internal(model_id, "unloaded").await;
        let speech = self.speech_engines.write().remove(model_id);
        if let Some(w) = speech {
            w.stop().await;
            self.emit("model_evicted", Some(model_id), "unloaded".to_string());
        }
    }

    async fn unload_internal(&self, model_id: &str, reason: &str) {
        let e = self.engines.write().remove(model_id);
        let gate = self.gates.write().remove(model_id);
        if let Some(gate) = gate {
            gate.close();
        }
        if let Some(e) = e {
            e.stop().await;
            self.emit("model_evicted", Some(model_id), reason.to_string());
        }
    }

    pub fn pin(&self, model_id: &str, pin: bool) {
        let mut pinned = self.pinned.write();
        if pin {
            pinned.insert(model_id.to_string());
        } else {
            pinned.remove(model_id);
        }
    }

    pub fn is_pinned(&self, model_id: &str) -> bool {
        self.pinned.read().contains(model_id)
    }

    /// The model's text engine, when one is loaded or loading.
    pub fn engine(&self, model_id: &str) -> Option<Arc<EngineProcess>> {
        self.engines.read().get(model_id).cloned()
    }

    // ------------------------------------------------------------------ speech

    /// None when speech is ready, otherwise a user-facing reason.
    pub fn speech_problem(&self) -> Option<String> {
        let registry = &self.config.registry;
        if registry
            .first_for_task("speech", self.config.catalog.default_speech_model())
            .is_none()
        {
            return Some(
                "No speech model is downloaded yet. Open Settings > Models and download Whisper."
                    .into(),
            );
        }
        if !self.is_component_installed(EngineComponent::Whisper) {
            return Some("The speech engine is not installed yet. Download a Whisper model from Settings > Models to install it.".into());
        }
        None
    }

    fn speech_model(&self, model_id: Option<&str>) -> Result<LocalModel> {
        let registry = &self.config.registry;
        model_id
            .and_then(|id| registry.find(id))
            .or_else(|| {
                registry.first_for_task("speech", self.config.catalog.default_speech_model())
            })
            .ok_or_else(|| anyhow!("No speech model is downloaded."))
    }

    /// Transcribes a 16 kHz mono WAV with the given or default speech model.
    pub async fn transcribe(&self, wav: &Path, model_id: Option<&str>) -> Result<String> {
        self.backend().await;
        if let Some(problem) = self.speech_problem() {
            bail!(problem);
        }
        let model = self.speech_model(model_id)?;
        let w = self.speech_engine(&model).await?;
        w.transcribe(wav, None).await
    }

    /// Like [`transcribe`](Self::transcribe) but returns the engine's verbose reply: `text`, the
    /// detected `language` and `segments` with `start`/`end` in seconds; `language` may be an ISO
    /// code or None for detection.
    pub async fn transcribe_detailed(
        &self,
        wav: &Path,
        model_id: Option<&str>,
        language: Option<&str>,
    ) -> Result<Value> {
        self.backend().await;
        if let Some(problem) = self.speech_problem() {
            bail!(problem);
        }
        let model = self.speech_model(model_id)?;
        let w = self.speech_engine(&model).await?;
        w.inference(wav, language, "verbose_json").await
    }

    async fn speech_engine(&self, model: &LocalModel) -> Result<Arc<WhisperProcess>> {
        if let Some(w) = self.speech_engines.read().get(&model.id) {
            if w.is_alive() {
                return Ok(w.clone());
            }
        }
        let _placement = self.placement.lock().await;
        let existing = self.speech_engines.read().get(&model.id).cloned();
        if let Some(w) = existing {
            if w.is_alive() {
                return Ok(w);
            }
            w.stop().await;
            self.speech_engines.write().remove(&model.id);
        }
        let b = self.backend().await;
        let exe = self.executable(EngineComponent::Whisper, b);
        let need = model.bytes * 2 + (512 << 20);
        if b != Backend::Cpu && self.config.inventory.budget_bytes().await < need {
            self.evict_idle(need, &model.id, false).await;
        }
        let fresh = Arc::new(WhisperProcess::new(
            &model.id,
            &model.file,
            &exe,
            &self.config.home.engine_logs_dir(),
            b == Backend::Cuda,
        )?);
        self.emit("model_loading", Some(&model.id), "speech".to_string());
        fresh.start(self.options.speech_start_timeout).await?;
        self.speech_engines
            .write()
            .insert(model.id.clone(), fresh.clone());
        self.emit(
            "model_loaded",
            Some(&model.id),
            format!("port={}", fresh.port()),
        );
        Ok(fresh)
    }

    // ------------------------------------------------------------------ images

    /// None when images can be generated, otherwise a user-facing reason.
    pub fn image_problem(&self) -> Option<String> {
        if self
            .config
            .registry
            .first_for_task("image", self.config.catalog.default_image_model())
            .is_none()
        {
            return Some(
                "No image model is downloaded yet. Open Settings > Models and download SD Turbo."
                    .into(),
            );
        }
        if !self.is_component_installed(EngineComponent::Sd) {
            return Some("The image engine is not installed yet. Download an image model from Settings > Models to install it.".into());
        }
        None
    }

    pub fn is_image_busy(&self) -> bool {
        self.image_busy.load(Ordering::SeqCst)
    }

    /// Renders one image. Image generation is exclusive: idle text and speech engines are evicted
    /// first when the model would not fit beside them, and only one render runs at a time.
    pub async fn generate_image(
        &self,
        model_id: Option<&str>,
        prompt: &str,
        negative: Option<&str>,
        width: Option<u32>,
        height: Option<u32>,
        steps: Option<u32>,
    ) -> Result<ImageResult> {
        let b = self.backend().await;
        if let Some(problem) = self.image_problem() {
            bail!(problem);
        }
        let registry = &self.config.registry;
        let model = model_id
            .and_then(|id| registry.find(id))
            .or_else(|| registry.first_for_task("image", self.config.catalog.default_image_model()))
            .ok_or_else(|| anyhow!("No image model is downloaded."))?;
        let defaults = self
            .config
            .catalog
            .find(&model.id)
            .map(|c| c.defaults.clone())
            .unwrap_or_default();
        let req = ImageEngine::request_for(&defaults, prompt, negative, width, height, steps);
        let engine = ImageEngine::new(
            &self.executable(EngineComponent::Sd, b),
            &self.config.home.engine_logs_dir(),
            &self.config.home.images_dir(),
        );

        let _image = self.image_lock.lock().await;
        let _busy = BusyFlag::set(&self.image_busy);
        let need = vram_need(&defaults, 4);
        if b != Backend::Cpu && self.config.inventory.budget_bytes().await < need {
            let _placement = self.placement.lock().await;
            let after = self.evict_idle(need, &model.id, true).await;
            if after < need {
                self.emit(
                    "image_tight_vram",
                    Some(&model.id),
                    format!(
                        "only {} MB free, needs about {} MB",
                        after >> 20,
                        need >> 20
                    ),
                );
            }
        }
        self.emit(
            "image_rendering",
            Some(&model.id),
            format!("{}x{} steps={}", req.width, req.height, req.steps),
        );
        let result = engine
            .generate(&model.file, &req, b != Backend::Cpu)
            .await?;
        self.emit(
            "image_rendered",
            Some(&model.id),
            format!(
                "{} in {} ms",
                result
                    .file
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                result.elapsed_ms
            ),
        );
        Ok(result)
    }

    // ------------------------------------------------------------------ video

    /// None when videos can be made, otherwise a user-facing reason.
    pub fn video_problem(&self) -> Option<String> {
        if self
            .config
            .registry
            .first_for_task("video", self.config.catalog.default_video_model())
            .is_none()
        {
            return Some("No video model is downloaded yet.".into());
        }
        if !self.is_component_installed(EngineComponent::Sd) {
            return Some(
                "The video engine is not installed yet. Download the video model to install it."
                    .into(),
            );
        }
        None
    }

    pub fn is_video_busy(&self) -> bool {
        self.video_busy.load(Ordering::SeqCst)
    }

    /// The installed model a clip would use: the one asked for, else the preferred or first video
    /// model.
    pub fn video_model(&self, model_id: Option<&str>) -> Option<LocalModel> {
        let registry = &self.config.registry;
        model_id
            .and_then(|id| registry.find(id))
            .filter(LocalModel::is_video)
            .or_else(|| registry.first_for_task("video", self.config.catalog.default_video_model()))
    }

    /// Renders one clip into `out`. Clips and images share one lock, as they share the sd engine
    /// and most of the card: idle text and speech engines are evicted first when the clip would
    /// not fit beside them, and only one render runs at a time. A clip waiting for the lock can
    /// still be stopped; a stopped clip fails with [`Stopped`].
    pub async fn generate_video(
        &self,
        model_id: Option<&str>,
        prompt: &str,
        out: &Path,
        progress: Option<VideoProgress>,
        cancel: &CancellationToken,
    ) -> Result<VideoResult> {
        let b = self.backend().await;
        if let Some(problem) = self.video_problem() {
            bail!(problem);
        }
        let model = self
            .video_model(model_id)
            .ok_or_else(|| anyhow!("No video model is downloaded."))?;
        let defaults = self
            .config
            .catalog
            .find(&model.id)
            .map(|c| c.defaults.clone())
            .unwrap_or_default();
        let req = VideoEngine::request_for(&defaults, prompt);
        let engine = VideoEngine::new(
            &self.executable(EngineComponent::Sd, b),
            &self.config.home.engine_logs_dir(),
        );

        let _image = tokio::select! {
            guard = self.image_lock.lock() => guard,
            _ = cancel.cancelled() => return Err(anyhow::Error::new(Stopped)),
        };
        let _busy = BusyFlag::set(&self.video_busy);
        let need = vram_need(&defaults, 7);
        if b != Backend::Cpu && self.config.inventory.budget_bytes().await < need {
            let _placement = self.placement.lock().await;
            let after = self.evict_idle(need, &model.id, true).await;
            if after < need {
                self.emit(
                    "video_tight_vram",
                    Some(&model.id),
                    format!(
                        "only {} MB free, needs about {} MB",
                        after >> 20,
                        need >> 20
                    ),
                );
            }
        }
        self.emit(
            "video_rendering",
            Some(&model.id),
            format!(
                "{}x{} frames={} steps={}",
                req.width, req.height, req.frames, req.steps
            ),
        );
        let result = engine
            .generate(&model.file, &req, out, progress, cancel)
            .await?;
        self.emit(
            "video_rendered",
            Some(&model.id),
            format!(
                "{} in {} s",
                result
                    .file
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                result.elapsed_ms / 1000
            ),
        );
        Ok(result)
    }

    /// A turn on the card for work outside the runtime's own engines (the flows' voice engine):
    /// it waits while an image or a clip renders, as they wait for it, and every idle text and
    /// speech engine that is not pinned is unloaded first. Not only as many as `need_bytes` seem
    /// to want: the voice engine's need is a guess, and on Windows a process short of video
    /// memory does not fail but pages through system memory and crawls (Vulkan always, CUDA
    /// under the NVIDIA driver's default sysmem fallback; a one-line clone took minutes beside a
    /// resident Whisper on an 8 GB card, 2026-09-26). The engines load again
    /// when the next run asks for them. The turn lasts as long as the guard; None when `cancel`
    /// fired while waiting.
    pub async fn gpu_turn(
        &self,
        need_bytes: u64,
        for_what: &str,
        cancel: &CancellationToken,
    ) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        let b = self.backend().await;
        let turn = tokio::select! {
            guard = self.image_lock.lock() => guard,
            _ = cancel.cancelled() => return None,
        };
        if b != Backend::Cpu {
            let _placement = self.placement.lock().await;
            let after = self.evict_all_idle(for_what).await;
            if after < need_bytes {
                tracing::info!(
                    "Only {} MB free on the card for {for_what}, which needs about {} MB",
                    after >> 20,
                    need_bytes >> 20
                );
            }
        }
        Some(turn)
    }

    /// Unloads every idle engine that is not pinned, text and speech alike, and returns what
    /// the card has free afterwards.
    async fn evict_all_idle(&self, for_what: &str) -> u64 {
        let reason = format!("evicted for {for_what}");
        let pinned = self.pinned.read().clone();
        let idle: Vec<String> = self
            .engines
            .read()
            .values()
            .filter(|e| e.in_flight() == 0 && !pinned.contains(e.model_id()))
            .map(|e| e.model_id().to_string())
            .collect();
        for id in idle {
            self.unload_internal(&id, &reason).await;
        }
        let speech: Vec<Arc<WhisperProcess>> = self
            .speech_engines
            .read()
            .values()
            .filter(|w| w.in_flight() == 0)
            .cloned()
            .collect();
        for w in speech {
            self.speech_engines.write().remove(w.model_id());
            w.stop().await;
            self.emit("model_evicted", Some(w.model_id()), reason.clone());
        }
        self.config.inventory.budget_bytes().await
    }

    // ------------------------------------------------------------------ housekeeping

    /// Removes crashed engines and unloads idle ones (not pinned ones).
    pub async fn sweep(&self) {
        let now = Utc::now();
        let ttl =
            chrono::Duration::from_std(self.options.idle_ttl).unwrap_or(chrono::Duration::MAX);
        let idle_reason = format!("idle for {} min", self.options.idle_ttl.as_secs() / 60);
        let engines: Vec<Arc<EngineProcess>> = self.engines.read().values().cloned().collect();
        for e in engines {
            if e.state() == State::Ready && !e.is_alive() {
                self.remove_engine_if_same(&e);
                e.stop().await;
                self.emit("engine_crashed", Some(e.model_id()), e.log_tail(10));
                continue;
            }
            let idle = e.in_flight() == 0 && now - e.last_used() > ttl;
            if idle && !self.is_pinned(e.model_id()) && e.state() == State::Ready {
                if let Ok(_placement) = self.placement.try_lock() {
                    self.unload_internal(e.model_id(), &idle_reason).await;
                }
            }
        }
        let speech: Vec<Arc<WhisperProcess>> =
            self.speech_engines.read().values().cloned().collect();
        for w in speech {
            if !w.is_alive() {
                self.speech_engines.write().remove(w.model_id());
                self.emit("engine_crashed", Some(w.model_id()), w.log_tail(10));
                continue;
            }
            let idle = w.in_flight() == 0 && now - w.last_used() > ttl;
            if idle {
                if let Ok(_placement) = self.placement.try_lock() {
                    self.speech_engines.write().remove(w.model_id());
                    w.stop().await;
                    self.emit("model_evicted", Some(w.model_id()), idle_reason.clone());
                }
            }
        }
    }

    fn remove_engine_if_same(&self, e: &Arc<EngineProcess>) {
        let mut engines = self.engines.write();
        if engines
            .get(e.model_id())
            .is_some_and(|current| Arc::ptr_eq(current, e))
        {
            engines.remove(e.model_id());
        }
    }

    /// Everything the Runtime and Models pages show.
    pub async fn status(&self) -> Status {
        let b = self.backend().await;
        let devices = self.config.inventory.snapshot().await.devices;
        let budget = gpu_inventory::budget_of(&devices);
        let local = self.config.registry.list();
        let by_id: HashMap<&str, &LocalModel> = local.iter().map(|m| (m.id.as_str(), m)).collect();
        let mut engines: Vec<Arc<EngineProcess>> = self.engines.read().values().cloned().collect();
        engines.sort_by(|a, b| a.model_id().cmp(b.model_id()));
        let infos = {
            let gates = self.gates.read();
            engines
                .iter()
                .map(|e| {
                    let m = by_id.get(e.model_id());
                    let g = gates.get(e.model_id());
                    EngineInfo {
                        model_id: e.model_id().to_string(),
                        display_name: m
                            .map(|m| m.display_name.clone())
                            .unwrap_or_else(|| e.model_id().to_string()),
                        state: e.state(),
                        port: e.port(),
                        gpu_layers: e.plan().gpu_layers,
                        layers: m
                            .and_then(|m| m.metadata.as_ref())
                            .map(|meta| meta.layers())
                            .unwrap_or(0),
                        ctx_per_slot: if e.engine_ctx_per_slot() > 0 {
                            e.engine_ctx_per_slot() as u32
                        } else {
                            e.plan().ctx_per_slot
                        },
                        slots: e.plan().slots,
                        in_flight: e.in_flight(),
                        pinned: self.is_pinned(e.model_id()),
                        started_at: e.started_at(),
                        last_used: e.last_used(),
                        failure: e.failure(),
                        active: g.map(|g| g.active()).unwrap_or(0),
                        waiting_interactive: g.map(|g| g.waiting_interactive()).unwrap_or(0),
                        waiting_background: g.map(|g| g.waiting_background()).unwrap_or(0),
                        tensor_split: e.plan().tensor_split.clone(),
                    }
                })
                .collect()
        };
        let mut speech: Vec<SpeechInfo> = self
            .speech_engines
            .read()
            .values()
            .map(|w| SpeechInfo {
                model_id: w.model_id().to_string(),
                port: w.port(),
                in_flight: w.in_flight(),
                last_used: w.last_used(),
            })
            .collect();
        speech.sort_by(|a, b| a.model_id.cmp(&b.model_id));
        let packages = &self.config.packages;
        Status {
            backend: b,
            engine_version: packages.version().to_string(),
            engine_installed: packages.is_installed(EngineComponent::Llama, b),
            speech_engine_installed: packages.is_installed(EngineComponent::Whisper, b),
            image_engine_installed: packages.is_installed(EngineComponent::Sd, b),
            devices,
            budget_bytes: budget,
            engines: infos,
            speech_engines: speech,
            image_busy: self.is_image_busy(),
            installed_models: local.iter().map(|m| m.id.clone()).collect(),
            readiness: self.readiness(),
            speech_engine_version: packages.version_of(EngineComponent::Whisper).to_string(),
            image_engine_version: packages.version_of(EngineComponent::Sd).to_string(),
            downloads: self.downloads(),
            recent_events: self
                .recent_events
                .lock()
                .iter()
                .take(STATUS_EVENTS)
                .cloned()
                .collect(),
            probes: self.probe.all(),
        }
    }

    /// Stops background work, downloads and every engine.
    pub async fn shutdown(&self) {
        self.stopping.cancel();
        let engines: Vec<Arc<EngineProcess>> =
            self.engines.write().drain().map(|(_, e)| e).collect();
        for gate in self.gates.write().drain().map(|(_, g)| g) {
            gate.close();
        }
        for e in engines {
            e.stop().await;
        }
        let speech: Vec<Arc<WhisperProcess>> = self
            .speech_engines
            .write()
            .drain()
            .map(|(_, w)| w)
            .collect();
        for w in speech {
            w.stop().await;
        }
    }
}

impl BusyWork for RuntimeManager {
    fn busy_with(&self) -> Option<String> {
        if self.downloads.lock().is_empty() {
            None
        } else {
            Some("a model is downloading".to_string())
        }
    }
}

#[async_trait]
impl WorkerRuntime for RuntimeManager {
    fn registry(&self) -> Arc<ModelRegistry> {
        self.config.registry.clone()
    }
    fn catalog(&self) -> Arc<ModelCatalog> {
        self.config.catalog.clone()
    }
    fn is_loaded(&self, model_id: &str) -> bool {
        self.engine(model_id).is_some()
    }
    fn context_per_request(&self, model_id: &str) -> u32 {
        RuntimeManager::context_per_request(self, model_id)
    }
    async fn acquire(&self, model_id: &str, priority: Priority) -> Result<Box<dyn api::Lease>> {
        let lease = RuntimeManager::acquire(self, model_id, priority).await?;
        Ok(Box::new(lease))
    }
    fn speech_problem(&self) -> Option<String> {
        RuntimeManager::speech_problem(self)
    }
    async fn transcribe(&self, wav: &Path, model_id: Option<&str>) -> Result<String> {
        RuntimeManager::transcribe(self, wav, model_id).await
    }
}

/// The runtime as the Video page's studio sees it (`video/VideoStudio.java` calls these three),
/// so the studio can be tested against a fake.
#[async_trait]
pub trait VideoRuntime: Send + Sync {
    /// Why clips cannot be made now, or None.
    fn video_problem(&self) -> Option<String>;
    /// The installed model a clip would use.
    fn video_model(&self, model_id: Option<&str>) -> Option<LocalModel>;
    /// Renders one clip into `out`; a stopped clip fails with [`Stopped`].
    async fn generate_video(
        &self,
        model_id: Option<&str>,
        prompt: &str,
        out: &Path,
        progress: Option<VideoProgress>,
        cancel: &CancellationToken,
    ) -> Result<VideoResult>;
}

#[async_trait]
impl VideoRuntime for RuntimeManager {
    fn video_problem(&self) -> Option<String> {
        RuntimeManager::video_problem(self)
    }
    fn video_model(&self, model_id: Option<&str>) -> Option<LocalModel> {
        RuntimeManager::video_model(self, model_id)
    }
    async fn generate_video(
        &self,
        model_id: Option<&str>,
        prompt: &str,
        out: &Path,
        progress: Option<VideoProgress>,
        cancel: &CancellationToken,
    ) -> Result<VideoResult> {
        RuntimeManager::generate_video(self, model_id, prompt, out, progress, cancel).await
    }
}

/// The load the title strip's mark shows while Nook works: the card's utilisation when it is
/// read, else the share of slots in use, else full (speech, images and loading have no slots);
/// never below [`MIN_BUSY_LOAD`].
pub fn busy_load(utilization: Option<f64>, active: u32, slots: u32) -> f64 {
    let load = match utilization {
        Some(u) => u / 100.0,
        None if slots > 0 && active > 0 => active as f64 / slots as f64,
        None => 1.0,
    };
    load.clamp(0.0, 1.0).max(MIN_BUSY_LOAD)
}

/// The model's pin for the speed probe: its checksum, else its size.
fn pin_of(model: &LocalModel) -> String {
    match model.sha256.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(sha) => sha.to_string(),
        None => format!("bytes:{}", model.bytes),
    }
}

/// The GPU memory a render needs from the catalog's `vramGb` (whole gigabytes), else `default_gb`.
fn vram_need(defaults: &BTreeMap<String, String>, default_gb: u64) -> u64 {
    defaults
        .get("vramGb")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default_gb)
        << 30
}

/// A runtime on a temporary home with fake engines, shared by the manager's and the download
/// service's tests.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::home::Home;
    use crate::runtime::downloader::Downloader;
    use crate::runtime::gguf_metadata::testing::{write_gguf_named, Kv};
    use axum::body::Body;
    use axum::response::{IntoResponse, Response};
    use axum::Router;
    use std::io::Write;

    pub(crate) struct Rig {
        pub dir: tempfile::TempDir,
        pub home: Home,
        pub bin: PathBuf,
        pub manager: Arc<RuntimeManager>,
    }

    pub(crate) struct RigSpec {
        /// Free memory the fake nvidia-smi reports (Windows); None: no GPU reading at all.
        pub free_mb: Option<u64>,
        pub backend: Backend,
        pub catalog: Option<String>,
        pub manifest: Option<String>,
        pub hub_api: Option<String>,
        pub options: RuntimeOptions,
    }

    impl Default for RigSpec {
        fn default() -> Self {
            RigSpec {
                free_mb: None,
                backend: Backend::Cuda,
                catalog: None,
                manifest: None,
                hub_api: None,
                options: RuntimeOptions {
                    idle_ttl: Duration::from_secs(3600),
                    start_timeout: Duration::from_secs(30),
                    speech_start_timeout: Duration::from_secs(30),
                    probe_delay: Duration::from_millis(50),
                    system_ram_bytes: Some(64 << 30),
                    ..RuntimeOptions::default()
                },
            }
        }
    }

    /// A fake nvidia-smi reporting one 8 GB card with `free_mb` free.
    #[cfg(windows)]
    fn write_smi(dir: &Path, free_mb: u64) -> PathBuf {
        let path = dir.join("nvidia-smi.cmd");
        std::fs::write(
            &path,
            format!("@echo off\r\necho 0, NVIDIA GeForce RTX 4060, 8188, {free_mb}, 560.94, 8.9, 40, 50\r\n"),
        )
        .unwrap();
        path
    }

    pub(crate) fn rig(spec: RigSpec) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path().join("home"));
        home.ensure_layout().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        #[cfg(windows)]
        let smi = match spec.free_mb {
            Some(free) => write_smi(&bin, free),
            None => bin.join("no-such-nvidia-smi.exe"),
        };
        #[cfg(not(windows))]
        let smi = bin.join("no-such-nvidia-smi");
        let downloader = Arc::new(Downloader::new());
        let catalog = Arc::new(match &spec.catalog {
            Some(json) => ModelCatalog::from_json(json).unwrap(),
            None => ModelCatalog::bundled().unwrap(),
        });
        let packages = Arc::new(match &spec.manifest {
            Some(json) => {
                EnginePackages::from_json(home.clone(), downloader.clone(), json).unwrap()
            }
            None => EnginePackages::new(home.clone(), downloader.clone()).unwrap(),
        });
        let registry = Arc::new(ModelRegistry::with_shared_dir(
            home.clone(),
            catalog.clone(),
            downloader.clone(),
            None,
        ));
        let hub = Arc::new(HuggingFaceHub::with_api(
            spec.hub_api.as_deref().unwrap_or("http://127.0.0.1:9"),
            home.clone(),
            downloader.clone(),
            None,
        ));
        let config = RuntimeConfig {
            home: home.clone(),
            downloader,
            catalog,
            inventory: Arc::new(GpuInventory::with_nvidia_smi(smi)),
            packages,
            registry,
            hub,
        };
        let mut options = spec.options;
        options.backend = Some(spec.backend);
        #[cfg(windows)]
        if options.executables.is_none() {
            use crate::runtime::engine_process::tests::fake_engine;
            let llama = fake_engine(&bin, "llama-server.cmd", None);
            let whisper = fake_engine(&bin, "whisper-server.cmd", None);
            let sd = crate::runtime::video_engine::tests::fake_sd(&bin.join("sd"));
            let other = bin.join("missing.exe");
            options.executables = Some(Arc::new(move |c, _| match c {
                EngineComponent::Llama => llama.clone(),
                EngineComponent::Whisper => whisper.clone(),
                EngineComponent::Sd => sd.clone(),
                EngineComponent::Ffmpeg
                | EngineComponent::Audio
                | EngineComponent::Pdfium
                | EngineComponent::Pandoc
                | EngineComponent::Office => other.clone(),
            }));
        }
        let manager = RuntimeManager::with_options(config, options);
        Rig {
            dir,
            home,
            bin,
            manager,
        }
    }

    /// Marks a component as installed for the rig's backend.
    pub(crate) fn install(rig: &Rig, component: EngineComponent) {
        let packages = rig.manager.packages();
        let b = rig.manager.backend_now();
        let dir = packages.dir(component, b);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("installed.json"),
            serde_json::json!({"version": packages.version_of(component), "component": component.id(), "backend": b.id()})
                .to_string(),
        )
        .unwrap();
    }

    /// A small llama-architecture model of `weights_mb` (8 layers, 4096 trained context); its id.
    pub(crate) fn text_model(
        rig: &Rig,
        file: &str,
        weights_mb: u64,
        extra: Vec<(&str, Kv)>,
    ) -> String {
        let mut kv = vec![
            ("general.architecture", Kv::Str("llama")),
            ("general.name", Kv::Str(file)),
            ("llama.block_count", Kv::U32(8)),
            ("llama.attention.head_count", Kv::U32(8)),
            ("llama.attention.head_count_kv", Kv::U32(8)),
            ("llama.embedding_length", Kv::U32(512)),
            ("llama.context_length", Kv::U32(4096)),
        ];
        kv.extend(extra);
        write_gguf_named(
            &rig.home.models_dir().join("test"),
            file,
            &kv,
            weights_mb << 20,
        );
        let registry = rig.manager.registry();
        registry.invalidate();
        registry
            .list()
            .into_iter()
            .find(|m| m.file.file_name().is_some_and(|n| n == file))
            .unwrap()
            .id
    }

    /// A non-GGUF model (speech, image, video) with its sidecar.
    pub(crate) fn sidecar_model(rig: &Rig, family: &str, file: &str, id: &str, task: &str) {
        let path = rig.home.models_dir().join(family).join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"weights").unwrap();
        std::fs::write(
            crate::runtime::model_registry::sidecar_of(&path),
            serde_json::json!({"id": id, "task": task, "displayName": id}).to_string(),
        )
        .unwrap();
        rig.manager.registry().invalidate();
    }

    /// Serves the HTTP side of every fake engine the rig starts: each start appends its arguments
    /// to `bin\args.txt`; a line with `--api-key` is a llama-server (its `/props` report
    /// `n_ctx`, or the plan's context per slot), any other a whisper-server.
    #[cfg(windows)]
    pub(crate) fn serve_fakes(bin: PathBuf, n_ctx: Option<i32>) -> tokio::task::JoinHandle<()> {
        use crate::runtime::engine_process::tests::fake_llama_server;
        use crate::runtime::whisper_process::tests::fake_whisper_server;
        tokio::spawn(async move {
            let mut served = 0;
            loop {
                if let Ok(text) = std::fs::read_to_string(bin.join("args.txt")) {
                    let lines: Vec<String> = text.lines().map(str::to_string).collect();
                    for line in lines.iter().skip(served) {
                        let args: Vec<&str> = line.split_whitespace().collect();
                        let after = |flag: &str| {
                            args.iter()
                                .position(|a| *a == flag)
                                .and_then(|i| args.get(i + 1))
                                .map(|s| s.to_string())
                        };
                        let Some(port) = after("--port").and_then(|p| p.parse::<u16>().ok()) else {
                            continue;
                        };
                        match after("--api-key") {
                            Some(key) => {
                                let total: i32 = after("--ctx-size")
                                    .and_then(|v| v.parse().ok())
                                    .unwrap_or(0);
                                let slots: i32 = after("--parallel")
                                    .and_then(|v| v.parse().ok())
                                    .unwrap_or(1);
                                let ctx = n_ctx.unwrap_or(total / slots.max(1));
                                fake_llama_server(port, Some(key), ctx, slots).await;
                            }
                            None => {
                                fake_whisper_server(port, Arc::new(Mutex::new(Vec::new()))).await
                            }
                        }
                    }
                    served = lines.len();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    }

    /// Waits up to ten seconds for a condition.
    pub(crate) async fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        for _ in 0..500 {
            if ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {what}");
    }

    /// The newest event of a kind for a model (any model when None).
    pub(crate) fn event(
        m: &RuntimeManager,
        kind: &str,
        model: Option<&str>,
    ) -> Option<RuntimeEvent> {
        m.recent_events()
            .into_iter()
            .find(|e| e.kind == kind && (model.is_none() || e.model_id.as_deref() == model))
    }

    pub(crate) fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            for (name, data) in entries {
                w.start_file(*name, opts).unwrap();
                w.write_all(data).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    /// Serves files by path, streamed in 16 KB chunks with `delay` between them; anything else is
    /// a 404.
    pub(crate) async fn serve_files(files: Vec<(String, Vec<u8>)>, delay: Duration) -> String {
        let files: Arc<HashMap<String, Vec<u8>>> = Arc::new(files.into_iter().collect());
        let router = Router::new().fallback(move |uri: axum::http::Uri| {
            let files = files.clone();
            async move {
                let Some(bytes) = files.get(uri.path()).cloned() else {
                    return (axum::http::StatusCode::NOT_FOUND, "missing").into_response();
                };
                let chunks: Vec<Vec<u8>> = bytes.chunks(16 << 10).map(<[u8]>::to_vec).collect();
                let stream = futures::stream::iter(chunks.into_iter().enumerate()).then(
                    move |(i, c)| async move {
                        if i > 0 {
                            tokio::time::sleep(delay).await;
                        }
                        Ok::<_, std::io::Error>(axum::body::Bytes::from(c))
                    },
                );
                Response::builder()
                    .body(Body::from_stream(stream))
                    .unwrap()
                    .into_response()
            }
        });
        crate::runtime::downloader::tests::serve(router).await
    }

    use futures::StreamExt;
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::runtime::gguf_metadata::testing::Kv;

    #[tokio::test]
    async fn interactive_goes_ahead_of_waiting_background() {
        let gate = Arc::new(EngineGate::new(1));
        gate.acquire(Priority::Background).await.unwrap(); // occupies the only slot

        let order = Arc::new(Mutex::new(Vec::<&str>::new()));
        let (g, o) = (gate.clone(), order.clone());
        let bg = tokio::spawn(async move {
            g.acquire(Priority::Background).await.unwrap();
            o.lock().push("background");
            g.release(Priority::Background);
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (g, o) = (gate.clone(), order.clone());
        let fg = tokio::spawn(async move {
            g.acquire(Priority::Interactive).await.unwrap();
            o.lock().push("interactive");
            g.release(Priority::Interactive);
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(gate.waiting_interactive(), 1);
        assert_eq!(gate.waiting_background(), 1);

        gate.release(Priority::Background); // free the slot
        fg.await.unwrap();
        bg.await.unwrap();
        assert_eq!(*order.lock(), ["interactive", "background"]);
    }

    #[tokio::test]
    async fn background_leaves_one_slot_free_when_there_are_several() {
        let gate = Arc::new(EngineGate::new(2));
        gate.acquire(Priority::Background).await.unwrap();
        assert_eq!(gate.active(), 1);

        let started = Arc::new(AtomicBool::new(false));
        let (g, s) = (gate.clone(), started.clone());
        let bg2 = tokio::spawn(async move {
            g.acquire(Priority::Background).await.unwrap();
            s.store(true, Ordering::SeqCst);
            g.release(Priority::Background);
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !started.load(Ordering::SeqCst),
            "second background request must wait for the reserved slot"
        );

        // an interactive request takes the reserved slot immediately
        gate.acquire(Priority::Interactive).await.unwrap();
        assert_eq!(gate.active(), 2);
        gate.release(Priority::Interactive);

        gate.release(Priority::Background);
        bg2.await.unwrap();
        assert!(started.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn closing_wakes_waiters_with_an_error() {
        let gate = Arc::new(EngineGate::new(1));
        gate.acquire(Priority::Interactive).await.unwrap();
        let g = gate.clone();
        let t = tokio::spawn(async move { g.acquire(Priority::Background).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        gate.close();
        let err = t.await.unwrap().unwrap_err();
        assert_eq!(
            err.to_string(),
            "The model was unloaded while the request was waiting."
        );
    }

    #[tokio::test]
    async fn a_waiter_that_gives_up_leaves_the_queue() {
        let gate = Arc::new(EngineGate::new(1));
        gate.acquire(Priority::Interactive).await.unwrap();
        let waited = tokio::time::timeout(
            Duration::from_millis(50),
            gate.acquire(Priority::Interactive),
        )
        .await;
        assert!(waited.is_err(), "still waiting when given up");
        assert_eq!(gate.waiting_interactive(), 0);
    }

    /// The mark in the title strip spins at the busiest card's share; a card with no reading
    /// does not count as idle.
    #[test]
    fn the_busiest_card_sets_the_marks_speed() {
        assert!((busy_load(Some(87.0), 1, 1) - 0.87).abs() < 1e-9);
        assert_eq!(
            busy_load(Some(3.0), 1, 1),
            MIN_BUSY_LOAD,
            "at work, it turns even while the card reads little"
        );
        assert_eq!(busy_load(Some(140.0), 1, 1), 1.0);
        assert_eq!(
            busy_load(None, 2, 4),
            0.5,
            "no reading: the share of slots in use"
        );
        assert_eq!(
            busy_load(None, 0, 4),
            1.0,
            "speech, an image or a load: full"
        );
    }

    #[tokio::test]
    async fn readiness_and_the_problems_say_what_is_missing() {
        let rig = rig(RigSpec::default());
        let m = &rig.manager;
        assert_eq!(m.readiness(), Readiness::EngineMissing);
        assert_eq!(
            m.speech_problem().as_deref(),
            Some("No speech model is downloaded yet. Open Settings > Models and download Whisper.")
        );
        assert_eq!(
            m.image_problem().as_deref(),
            Some("No image model is downloaded yet. Open Settings > Models and download SD Turbo.")
        );
        assert_eq!(
            m.video_problem().as_deref(),
            Some("No video model is downloaded yet.")
        );
        assert!(
            !m.needs_engine().await,
            "nothing installed: the first download brings it"
        );

        sidecar_model(&rig, "whisper", "ggml-small.bin", "whisper-small", "speech");
        sidecar_model(&rig, "sd", "sd_turbo.safetensors", "sd-turbo", "image");
        sidecar_model(
            &rig,
            "wan",
            "Wan2.1-T2V-1.3B-Q8_0.gguf",
            "wan2.1-t2v-1.3b",
            "video",
        );
        assert_eq!(
            m.speech_problem().as_deref(),
            Some("The speech engine is not installed yet. Download a Whisper model from Settings > Models to install it.")
        );
        assert_eq!(
            m.image_problem().as_deref(),
            Some("The image engine is not installed yet. Download an image model from Settings > Models to install it.")
        );
        assert_eq!(
            m.video_problem().as_deref(),
            Some("The video engine is not installed yet. Download the video model to install it.")
        );
        assert!(
            m.needs_engine().await,
            "models without the engine bring it back"
        );
        assert_eq!(m.video_model(None).unwrap().id, "wan2.1-t2v-1.3b");
        assert_eq!(
            m.video_model(Some("sd-turbo")).unwrap().id,
            "wan2.1-t2v-1.3b",
            "an image model is not a video model"
        );

        install(&rig, EngineComponent::Llama);
        install(&rig, EngineComponent::Whisper);
        install(&rig, EngineComponent::Sd);
        assert_eq!(m.readiness(), Readiness::NoModels);
        assert_eq!(m.speech_problem(), None);
        assert_eq!(m.image_problem(), None);
        assert_eq!(m.video_problem(), None);
        text_model(&rig, "Small-Chat-Q4.gguf", 1, vec![]);
        assert_eq!(m.readiness(), Readiness::Ready);
        assert!(!m.needs_engine().await);
        assert_eq!(m.backend().await, Backend::Cuda);

        let status = m.status().await;
        assert!(
            status.engine_installed
                && status.speech_engine_installed
                && status.image_engine_installed
        );
        assert_eq!(status.readiness, Readiness::Ready);
        assert_eq!(status.installed_models.len(), 4);
        assert!(status.devices.is_empty(), "no GPU reading");
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["backend"], "CUDA");
        assert_eq!(json["readiness"], "READY");
        assert!(json.get("speechEngineVersion").is_some() && json.get("recentEvents").is_some());
        m.shutdown().await;
    }

    #[tokio::test]
    async fn the_missing_engine_is_installed_with_the_note_hub_screen_showed() {
        let zip = zip_bytes(&[("llama-server.exe", b"server")]);
        let base = serve_files(vec![("/llama.zip".into(), zip)], Duration::ZERO).await;
        let manifest = format!(
            r#"{{"components":{{"llama":{{"version":"b9","backends":{{"cuda":[{{"name":"llama.zip","url":"{base}/llama.zip"}}]}}}}}}}}"#
        );
        let good = rig(RigSpec {
            manifest: Some(manifest),
            ..RigSpec::default()
        });
        text_model(&good, "Small-Chat-Q4.gguf", 1, vec![]);
        assert!(good.manager.needs_engine().await);
        assert_eq!(
            good.manager.install_missing_engine().await,
            "The Nook runtime (NVIDIA CUDA 12) is installed."
        );
        assert!(good.manager.is_engine_installed());
        let e = event(&good.manager, "engine_installed", None).unwrap();
        assert_eq!(e.detail.as_deref(), Some("llama b9 (cuda)"));

        let broken = rig(RigSpec {
            manifest: Some(format!(
                r#"{{"components":{{"llama":{{"version":"b9","backends":{{"cuda":[{{"name":"llama.zip","url":"{base}/gone.zip"}}]}}}}}}}}"#
            )),
            ..RigSpec::default()
        });
        let note = broken.manager.install_missing_engine().await;
        assert!(
            note.starts_with("Runtime install failed: HTTP 404"),
            "{note}"
        );
    }

    /// The whole path of a chat request: placement, the engine's start on its port with its key,
    /// the lease and its gate, the speed probe behind it, the status, and the unload.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_request_loads_the_model_on_a_fitting_plan_and_unloading_frees_it() {
        let rig = rig(RigSpec {
            free_mb: Some(8000),
            ..RigSpec::default()
        });
        let m = rig.manager.clone();
        install(&rig, EngineComponent::Llama);
        let id = text_model(&rig, "Small-Chat-Q4.gguf", 1000, vec![]);
        let fakes = serve_fakes(rig.bin.clone(), None);
        assert_eq!(
            m.context_per_request(&id),
            4096,
            "the plan's number before a load"
        );
        assert_eq!(m.context_per_request("nope"), 0);

        let lease = m.acquire(&id, Priority::Interactive).await.unwrap();
        let e = m.engine(&id).unwrap();
        assert_eq!(e.state(), State::Ready);
        assert_eq!(e.plan(), &Plan::new(999, 4096, 4, true, true));
        assert_eq!(
            event(&m, "model_loading", Some(&id))
                .unwrap()
                .detail
                .as_deref(),
            Some("ngl=999 ctx=4096 per request x 4 slots (--ctx-size 16384)")
        );
        let loaded = event(&m, "model_loaded", Some(&id))
            .unwrap()
            .detail
            .unwrap();
        assert!(
            loaded.ends_with("slots=4 ctx=4096 per request (engine confirms 4096 x 4)"),
            "{loaded}"
        );
        assert!(event(&m, "context_mismatch", None).is_none());
        assert!(
            m.gpu_load().await > 0.0,
            "a request in flight turns the mark"
        );
        assert!(!m.idle());
        let status = m.status().await;
        assert_eq!(status.engines.len(), 1);
        let info = &status.engines[0];
        assert_eq!(info.display_name, "Small-Chat-Q4.gguf");
        assert_eq!(
            (info.gpu_layers, info.layers, info.slots, info.ctx_per_slot),
            (999, 8, 4, 4096)
        );
        assert_eq!((info.active, info.in_flight), (1, 1));
        assert_eq!(
            status.budget_bytes,
            (8000 << 20) - gpu_inventory::DRIVER_RESERVE_BYTES
        );
        assert_eq!(status.devices[0].name, "NVIDIA GeForce RTX 4060");
        assert!(lease.client().health().await);
        drop(lease);
        assert_eq!(
            m.gates.read()[&id].active(),
            0,
            "the lease let go of its slot"
        );

        // The dyn seam the Code worker holds.
        let worker: Arc<dyn WorkerRuntime> = m.clone();
        assert!(worker.is_loaded(&id));
        let second = worker.acquire(&id, Priority::Background).await.unwrap();
        assert_eq!(
            second.client().base_url(),
            e.client().base_url(),
            "the same engine"
        );
        drop(second);

        wait_for("the speed probe", || {
            event(&m, "probe_measured", Some(&id)).is_some()
        })
        .await;
        assert_eq!(
            event(&m, "probe_measured", Some(&id))
                .unwrap()
                .detail
                .as_deref(),
            Some("generate 31.0 tok/s, prompt 812.3 tok/s")
        );
        let probe = m
            .probe()
            .current(&id, Some("bytes:1048576000"), Some("560.94"));
        assert!(probe.is_some(), "{:?}", m.probe().all());

        m.pin(&id, true);
        assert!(m.status().await.engines[0].pinned);
        m.unload(&id).await;
        assert!(m.engine(&id).is_none());
        assert!(!worker.is_loaded(&id));
        assert_eq!(
            event(&m, "model_evicted", Some(&id))
                .unwrap()
                .detail
                .as_deref(),
            Some("unloaded")
        );
        assert!(m.idle());
        assert_eq!(m.gpu_load().await, 0.0);
        m.shutdown().await;
        fakes.abort();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn the_engines_own_context_wins_and_a_difference_is_reported() {
        let rig = rig(RigSpec {
            free_mb: Some(8000),
            ..RigSpec::default()
        });
        let m = rig.manager.clone();
        install(&rig, EngineComponent::Llama);
        let id = text_model(&rig, "Small-Chat-Q4.gguf", 1000, vec![]);
        let fakes = serve_fakes(rig.bin.clone(), Some(2048));
        drop(m.acquire(&id, Priority::Interactive).await.unwrap());
        let mismatch = event(&m, "context_mismatch", Some(&id))
            .unwrap()
            .detail
            .unwrap();
        assert_eq!(
            mismatch,
            "the engine gives each request 2048 tokens of context, the plan said 4096"
        );
        assert_eq!(m.context_per_request(&id), 2048);
        assert_eq!(m.status().await.engines[0].ctx_per_slot, 2048);
        m.shutdown().await;
        fakes.abort();
    }

    #[tokio::test]
    async fn a_model_that_cannot_load_says_why() {
        let rig = rig(RigSpec::default());
        let m = &rig.manager;
        let err = m
            .acquire("x", Priority::Interactive)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "The Nook runtime is not installed yet.");
        install(&rig, EngineComponent::Llama);
        let err = m
            .acquire("x", Priority::Interactive)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Model 'x' is not downloaded.");
        sidecar_model(&rig, "whisper", "ggml-small.bin", "whisper-small", "speech");
        let err = m.load("whisper-small").await.unwrap_err().to_string();
        assert_eq!(
            err,
            "Model 'whisper-small' is a speech model, not a text model."
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_failed_start_is_an_event_and_leaves_nothing_behind() {
        let rig = rig(RigSpec {
            free_mb: Some(8000),
            options: RuntimeOptions {
                start_timeout: Duration::from_secs(1),
                ..RigSpec::default().options
            },
            ..RigSpec::default()
        });
        let m = &rig.manager;
        install(&rig, EngineComponent::Llama);
        let id = text_model(&rig, "Small-Chat-Q4.gguf", 100, vec![]);
        // nobody serves the engine's port: it never becomes healthy
        let err = m
            .acquire(&id, Priority::Interactive)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("Engine did not become healthy within 1 s."),
            "{err}"
        );
        assert!(m.engine(&id).is_none());
        let failed = event(m, "model_failed", Some(&id)).unwrap();
        assert_eq!(failed.detail.as_deref(), Some(err.as_str()));
        m.shutdown().await;
    }

    /// A rig whose text engine prints `lines` and exits with 1.
    #[cfg(windows)]
    fn failing_rig(lines: &[&str]) -> Rig {
        let rig = rig(RigSpec {
            free_mb: Some(8000),
            ..RigSpec::default()
        });
        crate::runtime::engine_process::tests::fake_engine_saying(
            &rig.bin,
            "llama-server.cmd",
            lines,
            1,
        );
        rig
    }

    /// How many times a rig's engines were started.
    #[cfg(windows)]
    fn starts(rig: &Rig) -> usize {
        std::fs::read_to_string(rig.bin.join("args.txt"))
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    /// The engine exited at load because it does not know the model's architecture (llama.cpp
    /// b10752 and the DeepSeek V4 vision encoder, 2026-09-26): the request fails once with that
    /// reason, and later requests fail the same way at once, without starting the engine again,
    /// until the model file changes.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_model_the_engine_refuses_fails_once_with_why_and_is_not_started_again() {
        use crate::runtime::engine_process::tests::UNKNOWN_ARCHITECTURE;
        let rig = failing_rig(UNKNOWN_ARCHITECTURE);
        let m = rig.manager.clone();
        install(&rig, EngineComponent::Llama);
        let id = text_model(&rig, "DeepSeek-V4-Flash-Q2_K.gguf", 100, vec![]);
        let why = "DeepSeek-V4-Flash-Q2_K.gguf can't run in Nook: the engine (llama.cpp b10752) doesn't know its model architecture 'deepseek4-vision'.";

        let err = m.acquire(&id, Priority::Interactive).await.unwrap_err();
        assert_eq!(err.to_string(), why);
        assert_eq!(format!("{err:#}"), why, "what the gateway answers");
        let load = err
            .downcast_ref::<ModelLoadError>()
            .expect("a ModelLoadError");
        assert!(load.permanent);
        assert_eq!(starts(&rig), 1);
        assert!(m.engine(&id).is_none());
        assert_eq!(
            event(&m, "model_failed", Some(&id))
                .unwrap()
                .detail
                .as_deref(),
            Some(why)
        );

        for _ in 0..3 {
            let again = m.acquire(&id, Priority::Interactive).await.unwrap_err();
            assert_eq!(again.to_string(), why);
        }
        assert_eq!(
            m.load(&id).await.unwrap_err().to_string(),
            why,
            "the Runtime page's Load"
        );
        assert_eq!(starts(&rig), 1, "not started again");

        // A new file (or a new engine) is worth a try.
        let file = m.registry().find(&id).unwrap().file;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        m.registry().invalidate();
        assert_eq!(m.load(&id).await.unwrap_err().to_string(), why);
        assert_eq!(starts(&rig), 2);
        m.shutdown().await;
    }

    /// Running out of memory is worth another try once memory comes free: no refusal is kept.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_model_that_ran_out_of_memory_is_tried_again() {
        let rig = failing_rig(&[
            "ggml_backend_cuda_buffer_type_alloc_buffer: allocating 12345.67 MiB on device 0: cudaMalloc failed: out of memory",
            "llama_model_load: error loading model: unable to allocate CUDA0 buffer",
        ]);
        let m = rig.manager.clone();
        install(&rig, EngineComponent::Llama);
        let id = text_model(&rig, "Big-Q4_K_M.gguf", 100, vec![]);
        let err = m.acquire(&id, Priority::Interactive).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Big-Q4_K_M.gguf could not be loaded: the GPU ran out of memory. Close other applications that use it, or pick a smaller model."
        );
        assert!(!err.downcast_ref::<ModelLoadError>().unwrap().permanent);
        let _ = m.acquire(&id, Priority::Interactive).await.unwrap_err();
        assert_eq!(starts(&rig), 2);
        m.shutdown().await;
    }

    /// Every load failure the log names reads as a sentence about the model; a long reason is cut.
    #[test]
    fn a_load_failure_reads_as_a_sentence() {
        let refused = ModelLoadError::from_engine(
            "Qwen3.8-Flash-Next Q4_K_M",
            "llama.cpp b10752",
            &LoadFailure::Refused(Some(
                "check_tensor_dims: tensor 'token_embd.weight' not found".into(),
            )),
        );
        assert_eq!(
            refused.message,
            "Qwen3.8-Flash-Next Q4_K_M can't run in Nook: the engine (llama.cpp b10752) could not load it (check_tensor_dims: tensor 'token_embd.weight' not found)."
        );
        assert!(refused.permanent);
        let long = ModelLoadError::from_engine(
            "M",
            "llama.cpp b1",
            &LoadFailure::Refused(Some("x".repeat(400))),
        );
        assert!(long.message.chars().count() < 250, "{}", long.message);
        let bare = ModelLoadError::from_engine("M", "llama.cpp b1", &LoadFailure::Refused(None));
        assert_eq!(
            bare.message,
            "M could not be loaded: the engine (llama.cpp b1) stopped while it loaded it."
        );
        assert!(!bare.permanent);
    }

    /// A GGUF from the Hub whose header is no language model's is refused before any engine
    /// starts, with what it is; it is no chat model, so the runtime is not ready for chat either.
    #[tokio::test]
    async fn a_file_that_is_not_a_language_model_is_never_handed_to_the_engine() {
        use crate::runtime::gguf_metadata::testing::write_gguf_named;
        let rig = rig(RigSpec::default());
        let m = &rig.manager;
        install(&rig, EngineComponent::Llama);
        let hub = rig
            .home
            .models_dir()
            .join("hub")
            .join("antirez-deepseek-v4-gguf");
        let file = write_gguf_named(
            &hub,
            "DeepSeek-V4-Flash-Vision-Encoder.gguf",
            &[
                ("general.architecture", Kv::Str("deepseek4-vision")),
                ("deepseek4-vision.block_count", Kv::U32(32)),
                ("deepseek4-vision.embedding_length", Kv::U32(1024)),
            ],
            0,
        );
        std::fs::write(
            crate::runtime::model_registry::sidecar_of(&file),
            r#"{"id":"encoder","displayName":"Vision Encoder","task":"chat","source":"huggingface","role":"model"}"#,
        )
        .unwrap();
        m.registry().invalidate();
        assert_eq!(m.readiness(), Readiness::NoModels);
        let err = m.load("encoder").await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Vision Encoder can't run in Nook: its architecture 'deepseek4-vision' is a vision encoder or projector, not a language model."
        );
        assert!(err.downcast_ref::<ModelLoadError>().unwrap().permanent);
        assert_eq!(
            event(m, "model_failed", Some("encoder")).unwrap().detail,
            Some(err.to_string())
        );
        assert!(m.engine("encoder").is_none());
        assert!(!rig.bin.join("args.txt").exists(), "no engine was started");
    }

    /// Placement under tight memory, on the CPU, on Vulkan without a reading, and for a
    /// mixture-of-experts model (the planner's branches, without starting anything).
    #[cfg(windows)]
    #[tokio::test]
    async fn the_plan_follows_the_memory_there_is() {
        // 1200 MB free, 688 MB of budget; the model needs 1290 MB.
        let tight = rig(RigSpec {
            free_mb: Some(1200),
            ..RigSpec::default()
        });
        let id = text_model(&tight, "Big-Chat-Q4.gguf", 1000, vec![]);
        let model = tight.manager.registry().find(&id).unwrap();
        let plan = tight.manager.plan(&model).await;
        assert_eq!(plan, Plan::new(3, 4096, 2, true, true));
        assert_eq!(
            event(&tight.manager, "partial_offload", Some(&id)).unwrap().detail.as_deref(),
            Some("3 of 8 layers on GPU with 2 slots of 4096 context each (--ctx-size 8192; 688 MB free, full model needs 1290 MB). Close other GPU applications to load it fully.")
        );

        let none = rig(RigSpec {
            free_mb: Some(600),
            ..RigSpec::default()
        });
        let id = text_model(&none, "Big-Chat-Q4.gguf", 1000, vec![]);
        let model = none.manager.registry().find(&id).unwrap();
        assert_eq!(
            none.manager.plan(&model).await,
            Plan::new(0, 4096, 1, false, false)
        );
        assert_eq!(
            event(&none.manager, "cpu_fallback", Some(&id)).unwrap().detail.as_deref(),
            Some("no GPU memory available (88 MB free, model needs 1290 MB); running on the CPU. Close other GPU applications for speed.")
        );

        let cpu = rig(RigSpec {
            backend: Backend::Cpu,
            ..RigSpec::default()
        });
        let id = text_model(&cpu, "Big-Chat-Q4.gguf", 1000, vec![]);
        let model = cpu.manager.registry().find(&id).unwrap();
        assert_eq!(
            cpu.manager.plan(&model).await,
            Plan::new(0, 4096, 1, false, false)
        );

        let vulkan = rig(RigSpec {
            backend: Backend::Vulkan,
            ..RigSpec::default()
        });
        let id = text_model(&vulkan, "Big-Chat-Q4.gguf", 1000, vec![]);
        let model = vulkan.manager.registry().find(&id).unwrap();
        assert_eq!(
            vulkan.manager.plan(&model).await,
            Plan::new(-1, 4096, 1, false, false),
            "no reading yet: the engine fits itself, one slot"
        );

        let moe_rig = rig(RigSpec {
            free_mb: Some(1200),
            ..RigSpec::default()
        });
        let id = text_model(
            &moe_rig,
            "Moe-Q4.gguf",
            1000,
            vec![("llama.expert_count", Kv::U32(128))],
        );
        let model = moe_rig.manager.registry().find(&id).unwrap();
        let plan = moe_rig.manager.plan(&model).await;
        assert_eq!(
            plan,
            Plan::new(999, 4096, 2, true, true).with_experts_in_ram()
        );
        assert_eq!(
            event(&moe_rig.manager, "experts_in_ram", Some(&id)).unwrap().detail.as_deref(),
            Some("128 experts per layer in system memory, attention on the GPU (688 MB free, whole model 1290 MB)")
        );

        let small_ram = rig(RigSpec {
            free_mb: Some(1200),
            options: RuntimeOptions {
                system_ram_bytes: Some(16 << 30),
                ..RigSpec::default().options
            },
            ..RigSpec::default()
        });
        let id = text_model(
            &small_ram,
            "Moe-Q4.gguf",
            1000,
            vec![("llama.expert_count", Kv::U32(128))],
        );
        let model = small_ram.manager.registry().find(&id).unwrap();
        small_ram.manager.plan(&model).await;
        let events: Vec<String> = small_ram
            .manager
            .recent_events()
            .into_iter()
            .filter(|e| e.kind == "partial_offload")
            .filter_map(|e| e.detail)
            .collect();
        assert!(
            events.iter().any(|d| d == "a mixture-of-experts model needs about 24 GB of system memory to keep its experts in RAM; this machine has 16 GB"),
            "{events:?}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn workers_json_picks_the_context_and_the_slots_within_what_fits() {
        let rig = rig(RigSpec {
            free_mb: Some(8000),
            ..RigSpec::default()
        });
        let id = text_model(&rig, "Small-Chat-Q4.gguf", 1000, vec![]);
        std::fs::write(
            rig.manager.registry().workers_file(),
            r#"{"ctx": 2048, "slots": "2"}"#,
        )
        .unwrap();
        let model = rig.manager.registry().find(&id).unwrap();
        let plan = rig.manager.plan(&model).await;
        assert_eq!((plan.ctx_per_slot, plan.slots), (2048, 2));
        assert_eq!(
            event(&rig.manager, "ctx_override", Some(&id))
                .unwrap()
                .detail
                .as_deref(),
            Some("2048 tokens per request from workers.json (the catalog says 4096)")
        );
        assert_eq!(
            event(&rig.manager, "slots_override", Some(&id))
                .unwrap()
                .detail
                .as_deref(),
            Some("2 slots from workers.json (the plan would run 4)")
        );
        assert_eq!(rig.manager.context_per_request(&id), 2048);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn an_idle_model_makes_room_unless_it_is_pinned() {
        // 1288 MB of budget: the 600 MB model fits alone, the 1000 MB one needs it gone.
        let rig = rig(RigSpec {
            free_mb: Some(1800),
            ..RigSpec::default()
        });
        let m = rig.manager.clone();
        install(&rig, EngineComponent::Llama);
        let small = text_model(&rig, "Small-Chat-Q4.gguf", 600, vec![]);
        let big = text_model(&rig, "Big-Chat-Q4.gguf", 1000, vec![]);
        let fakes = serve_fakes(rig.bin.clone(), None);

        m.pin(&small, true);
        drop(m.acquire(&small, Priority::Interactive).await.unwrap());
        drop(m.acquire(&big, Priority::Interactive).await.unwrap());
        assert!(m.engine(&small).is_some(), "a pinned model stays");
        assert!(event(&m, "model_evicted", Some(&small)).is_none());
        m.unload(&big).await;

        m.pin(&small, false);
        drop(m.acquire(&big, Priority::Interactive).await.unwrap());
        assert!(m.engine(&small).is_none());
        assert_eq!(
            event(&m, "model_evicted", Some(&small)).unwrap().detail,
            Some(format!("evicted for {big}"))
        );
        m.shutdown().await;
        fakes.abort();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn the_voice_engine_gets_the_card_but_a_pinned_model_stays() {
        // Plenty of room by the numbers: the idle model goes all the same.
        let rig = rig(RigSpec {
            free_mb: Some(8000),
            ..RigSpec::default()
        });
        let m = rig.manager.clone();
        install(&rig, EngineComponent::Llama);
        let kept = text_model(&rig, "Kept-Chat-Q4.gguf", 600, vec![]);
        let idle = text_model(&rig, "Idle-Chat-Q4.gguf", 600, vec![]);
        let fakes = serve_fakes(rig.bin.clone(), None);
        m.pin(&kept, true);
        drop(m.acquire(&kept, Priority::Interactive).await.unwrap());
        drop(m.acquire(&idle, Priority::Interactive).await.unwrap());
        assert!(m.engine(&idle).is_some());

        let turn = m
            .gpu_turn(100 << 20, "the voice engine", &CancellationToken::new())
            .await
            .expect("a turn");
        assert!(
            m.engine(&idle).is_none(),
            "an idle model leaves the card to the voice"
        );
        assert_eq!(
            event(&m, "model_evicted", Some(&idle)).unwrap().detail,
            Some("evicted for the voice engine".to_string())
        );
        assert!(m.engine(&kept).is_some(), "a pinned model stays");
        drop(turn);
        m.shutdown().await;
        fakes.abort();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn the_sweep_clears_crashed_engines_and_unloads_idle_ones() {
        let rig = rig(RigSpec {
            free_mb: Some(8000),
            options: RuntimeOptions {
                idle_ttl: Duration::ZERO,
                probe_delay: Duration::from_secs(3600),
                ..RigSpec::default().options
            },
            ..RigSpec::default()
        });
        let m = rig.manager.clone();
        install(&rig, EngineComponent::Llama);
        let a = text_model(&rig, "Small-Chat-Q4.gguf", 100, vec![]);
        let b = text_model(&rig, "Other-Chat-Q4.gguf", 100, vec![]);
        let fakes = serve_fakes(rig.bin.clone(), None);

        let lease = m.acquire(&a, Priority::Interactive).await.unwrap();
        m.sweep().await;
        assert!(m.engine(&a).is_some(), "busy: not idle");
        m.engine(&a).unwrap().crash_for_test().await;
        m.sweep().await;
        assert!(m.engine(&a).is_none());
        let crashed = event(&m, "engine_crashed", Some(&a)).unwrap();
        assert!(
            crashed.detail.unwrap().contains("fake engine"),
            "the log's tail"
        );
        drop(lease);

        drop(m.acquire(&b, Priority::Interactive).await.unwrap());
        m.pin(&b, true);
        m.sweep().await;
        assert!(m.engine(&b).is_some(), "pinned: kept however idle");
        m.pin(&b, false);
        m.sweep().await;
        assert!(m.engine(&b).is_none());
        assert_eq!(
            event(&m, "model_evicted", Some(&b))
                .unwrap()
                .detail
                .as_deref(),
            Some("idle for 0 min")
        );
        m.shutdown().await;
        fakes.abort();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn speech_starts_its_engine_once_and_transcribes() {
        let rig = rig(RigSpec {
            free_mb: Some(8000),
            ..RigSpec::default()
        });
        let m = rig.manager.clone();
        install(&rig, EngineComponent::Whisper);
        sidecar_model(&rig, "whisper", "ggml-small.bin", "whisper-small", "speech");
        let fakes = serve_fakes(rig.bin.clone(), None);
        let wav = rig.dir.path().join("prompt.wav");
        std::fs::write(&wav, b"RIFF").unwrap();

        assert_eq!(m.transcribe(&wav, None).await.unwrap(), "hello there");
        let detailed = m
            .transcribe_detailed(&wav, Some("whisper-small"), Some("en"))
            .await
            .unwrap();
        assert_eq!(detailed["segments"][0]["end"], 1.5);
        let worker: Arc<dyn WorkerRuntime> = m.clone();
        assert_eq!(worker.transcribe(&wav, None).await.unwrap(), "hello there");
        assert_eq!(
            m.recent_events()
                .iter()
                .filter(|e| e.kind == "model_loading")
                .count(),
            1,
            "one engine for every transcription"
        );
        assert_eq!(
            event(&m, "model_loading", Some("whisper-small"))
                .unwrap()
                .detail
                .as_deref(),
            Some("speech")
        );
        let status = m.status().await;
        assert_eq!(status.speech_engines.len(), 1);
        assert_eq!(status.speech_engines[0].model_id, "whisper-small");
        m.unload("whisper-small").await;
        assert!(m.status().await.speech_engines.is_empty());
        assert_eq!(
            event(&m, "model_evicted", Some("whisper-small"))
                .unwrap()
                .detail
                .as_deref(),
            Some("unloaded")
        );
        m.shutdown().await;
        fakes.abort();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn images_and_clips_render_one_at_a_time() {
        let rig = rig(RigSpec {
            free_mb: Some(8000),
            ..RigSpec::default()
        });
        let m = rig.manager.clone();
        install(&rig, EngineComponent::Sd);
        sidecar_model(&rig, "sd", "sd_turbo.safetensors", "sd-turbo", "image");
        sidecar_model(
            &rig,
            "wan",
            "Wan2.1-T2V-1.3B-Q8_0.gguf",
            "wan2.1-t2v-1.3b",
            "video",
        );

        let image = m
            .generate_image(None, "a fox", None, Some(256), None, None)
            .await
            .unwrap();
        assert!(image.file.starts_with(rig.home.images_dir()) && image.file.is_file());
        assert_eq!(
            event(&m, "image_rendering", Some("sd-turbo"))
                .unwrap()
                .detail
                .as_deref(),
            Some("256x512 steps=4")
        );
        assert!(event(&m, "image_rendered", Some("sd-turbo")).is_some());
        assert!(!m.is_image_busy());

        let out = rig.home.videos_dir().join("clip.avi");
        let stages = Arc::new(Mutex::new(Vec::new()));
        let s = stages.clone();
        let progress: VideoProgress = Arc::new(move |stage, _, _| s.lock().push(stage));
        let clip = m
            .generate_video(
                None,
                "a fox",
                &out,
                Some(progress),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(clip.file, out);
        assert_eq!(clip.request.frames, 33);
        assert_eq!(
            event(&m, "video_rendering", Some("wan2.1-t2v-1.3b"))
                .unwrap()
                .detail
                .as_deref(),
            Some("832x480 frames=33 steps=20")
        );
        assert!(stages.lock().contains(&crate::runtime::VideoStage::Saving));
        assert!(!m.is_video_busy());

        // A clip waiting behind another render can still be stopped.
        let held = m.image_lock.lock().await;
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c.cancel();
        });
        let err = m
            .generate_video(
                None,
                "a fox",
                &rig.home.videos_dir().join("x.avi"),
                None,
                &cancel,
            )
            .await
            .unwrap_err();
        assert!(err.is::<Stopped>());
        drop(held);
        let studio: Arc<dyn VideoRuntime> = m.clone();
        assert_eq!(studio.video_problem(), None);
        m.shutdown().await;
    }

    /// A catalog download that installs the engine it needs first, on one progress scale, as
    /// the Models page and an agent start it.
    #[tokio::test]
    async fn a_catalog_download_brings_its_engine_and_reports_progress() {
        use sha2::Digest;
        let weights: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let zip = zip_bytes(&[("whisper-server.exe", b"w")]);
        let sha = hex::encode(sha2::Sha256::digest(&weights));
        let base = serve_files(
            vec![
                ("/tiny.bin".into(), weights.clone()),
                ("/whisper.zip".into(), zip.clone()),
            ],
            Duration::from_millis(30),
        )
        .await;
        let catalog = format!(
            r#"{{"defaultSpeechModel":"tiny-whisper","models":[
                {{"id":"tiny-whisper","displayName":"Tiny Whisper","family":"whisper","task":"speech",
                  "artifacts":[{{"file":"ggml-tiny.bin","url":"{base}/tiny.bin","sha256":"{sha}","bytes":{}}}]}},
                {{"id":"gone","family":"x","task":"speech","artifacts":[{{"file":"gone.bin","url":"{base}/gone.bin"}}]}}]}}"#,
            weights.len()
        );
        let manifest = format!(
            r#"{{"components":{{"whisper":{{"version":"w1","backends":{{"cuda":[{{"name":"whisper.zip","url":"{base}/whisper.zip","bytes":{}}}]}}}}}}}}"#,
            zip.len()
        );
        let rig = rig(RigSpec {
            catalog: Some(catalog),
            manifest: Some(manifest),
            ..RigSpec::default()
        });
        let m = rig.manager.clone();
        assert_eq!(m.busy_with(), None);
        assert!(m.download_async("tiny-whisper").unwrap());
        assert!(
            !m.download_async("tiny-whisper").unwrap(),
            "already downloading"
        );
        assert_eq!(m.busy_with().as_deref(), Some("a model is downloading"));
        assert!(m.downloads().contains_key("tiny-whisper"));
        assert_eq!(
            m.download_async("nope").unwrap_err().to_string(),
            "Unknown catalog model nope"
        );
        wait_for("the download", || m.downloads().is_empty()).await;
        assert!(
            event(&m, "model_downloaded", Some("tiny-whisper")).is_some(),
            "{:?}",
            m.recent_events()
        );
        assert_eq!(
            event(&m, "engine_installing", Some("tiny-whisper"))
                .unwrap()
                .detail
                .as_deref(),
            Some("whisper")
        );
        assert_eq!(
            event(&m, "engine_installed", None)
                .unwrap()
                .detail
                .as_deref(),
            Some("whisper w1")
        );
        let progress = event(&m, "download_progress", Some("tiny-whisper"))
            .unwrap()
            .detail
            .unwrap();
        assert!(
            progress.ends_with("MB)") && progress.contains('%'),
            "{progress}"
        );
        assert!(m.registry().is_installed("tiny-whisper"));
        assert!(m.is_component_installed(EngineComponent::Whisper));
        assert_eq!(m.speech_problem(), None);
        assert_eq!(m.busy_with(), None);

        assert!(m.download_async("gone").unwrap());
        wait_for("the failure", || m.downloads().is_empty()).await;
        let failed = event(&m, "download_failed", Some("gone"))
            .unwrap()
            .detail
            .unwrap();
        assert!(failed.starts_with("HTTP 404"), "{failed}");
    }

    #[tokio::test]
    async fn an_interrupted_download_is_picked_up() {
        let weights = vec![7u8; 40_000];
        let base = serve_files(vec![("/tiny.bin".into(), weights.clone())], Duration::ZERO).await;
        let catalog = format!(
            r#"{{"models":[{{"id":"tiny-whisper","family":"whisper","task":"speech",
                "artifacts":[{{"file":"ggml-tiny.bin","url":"{base}/tiny.bin","bytes":{}}}]}}]}}"#,
            weights.len()
        );
        let rig = rig(RigSpec {
            catalog: Some(catalog),
            ..RigSpec::default()
        });
        install(&rig, EngineComponent::Whisper);
        let part = rig
            .home
            .models_dir()
            .join("whisper")
            .join("ggml-tiny.bin.part");
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        std::fs::write(&part, &weights[..1000]).unwrap();
        let m = rig.manager.clone();
        m.resume_interrupted_downloads();
        assert_eq!(
            event(&m, "download_resumed", Some("tiny-whisper"))
                .unwrap()
                .detail
                .as_deref(),
            Some("picking up an interrupted download")
        );
        wait_for("the download", || m.downloads().is_empty()).await;
        assert!(m.registry().is_installed("tiny-whisper"));
        m.resume_interrupted_downloads();
        assert_eq!(
            m.recent_events()
                .iter()
                .filter(|e| e.kind == "download_resumed")
                .count(),
            1,
            "an installed model is not picked up again"
        );
    }

    #[tokio::test]
    async fn a_hub_download_lands_as_an_installed_model_and_can_be_stopped() {
        use crate::runtime::gguf_metadata::testing::write_language_model;
        use crate::runtime::hugging_face_hub::HubFile;
        let scratch = tempfile::tempdir().unwrap();
        let file = write_language_model(
            scratch.path(),
            "Tiny-Q4_K_M.gguf",
            "llama",
            &[("general.name", Kv::Str("Tiny"))],
            200_000,
        );
        let bytes = std::fs::read(&file).unwrap();
        let base = serve_files(
            vec![
                (
                    "/acme/Tiny-GGUF/resolve/main/Tiny-Q4_K_M.gguf".into(),
                    bytes.clone(),
                ),
                (
                    "/acme/Slow-GGUF/resolve/main/Slow-Q4_K_M.gguf".into(),
                    bytes.clone(),
                ),
            ],
            Duration::from_millis(40),
        )
        .await;
        let rig = rig(RigSpec {
            hub_api: Some(base),
            ..RigSpec::default()
        });
        install(&rig, EngineComponent::Llama);
        let m = rig.manager.clone();
        let repo = |name: &str| Repo {
            id: format!("acme/{name}"),
            author: "acme".into(),
            name: name.into(),
            downloads: 1,
            likes: 1,
            last_modified: None,
            gated: false,
            pipeline_tag: None,
            tags: vec![],
        };
        let variant = |key: &str| Variant {
            label: "Q4_K_M".into(),
            key: key.into(),
            files: vec![HubFile {
                path: format!("{key}.gguf"),
                bytes: bytes.len() as u64,
                sha256: None,
                quant: "Q4_K_M".into(),
                shard_index: 0,
                shard_count: 1,
                shard_base: key.into(),
            }],
            total_bytes: bytes.len() as u64,
        };
        let tiny = repo("Tiny-GGUF");
        let key = RuntimeManager::hub_key("acme/Tiny-GGUF", "Tiny-Q4_K_M");
        assert_eq!(key, "hub:acme/Tiny-GGUF:Tiny-Q4_K_M");
        assert!(m.download_hub_async(tiny.clone(), variant("Tiny-Q4_K_M")));
        assert!(!m.download_hub_async(tiny.clone(), variant("Tiny-Q4_K_M")));
        wait_for("the Hub download", || m.downloads().is_empty()).await;
        let done = event(&m, "model_downloaded", Some(&key)).unwrap();
        let id = hugging_face_hub::model_id(&tiny, &variant("Tiny-Q4_K_M"));
        assert_eq!(done.detail.as_deref(), Some(id.as_str()));
        assert!(m.registry().is_installed(&id), "{:?}", m.registry().list());

        let slow = repo("Slow-GGUF");
        let slow_key = RuntimeManager::hub_key("acme/Slow-GGUF", "Slow-Q4_K_M");
        assert!(m.download_hub_async(slow.clone(), variant("Slow-Q4_K_M")));
        wait_for("some progress", || {
            m.downloads().get(&slow_key).is_some_and(|p| *p > 0.0)
        })
        .await;
        m.cancel_hub_download("acme/Slow-GGUF", "Slow-Q4_K_M");
        wait_for("the stop", || m.downloads().is_empty()).await;
        assert!(event(&m, "download_cancelled", Some(&slow_key)).is_some());
        assert!(
            m.hub()
                .folder_for("acme/Slow-GGUF")
                .join("Slow-Q4_K_M.gguf.part")
                .exists(),
            "kept for a resume"
        );
    }
}
