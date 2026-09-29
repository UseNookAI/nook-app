//! The app's services, built once at start (the Kotlin app's Spring context).
//!
//! [`Nook::new`] builds every service without touching the network or starting a process (first
//! importing the Kotlin Nook's data into a new home, [`crate::migrate`]; the Code service reads its
//! saved sessions there and marks a run Nook closed on as ended, as the original's constructor
//! did); [`Nook::start`] runs what the original's beans started once the
//! window was up (the runtime's sweep and download resume, the loopback gateway, the updater's
//! schedule); [`Nook::shutdown`] stops them before exit: the gateway, Code turns and the video
//! studio first, the engines last.
//!
//! No unit test builds a whole `Nook`: the runtime manager's GPU inventory asks the real driver
//! (`nvidia-smi`) as soon as it exists, which tests must not do. The services are tested on their
//! own; the wiring is checked by running the app.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::capture::CaptureService;
use crate::code::CodeService;
use crate::convert::ConvertService;
use crate::flow::FlowService;
use crate::gateway::{Gateway, GatewayHandle};
use crate::nooklets::Finder;
use crate::pdf::{PdfEditor, PdfInstaller};
use crate::runtime::api::WorkerRuntime;
use crate::runtime::{Backend, EngineComponent};
use crate::runtime::{ModelDownloadService, RuntimeConfig, RuntimeManager};
use crate::settings::Settings;
use crate::speech::Recorder;
use crate::update::{ChannelStore, UpdateSource, Updater};
use crate::video::VideoStudio;
use crate::web::WebAccess;
use crate::Home;

/// Everything the UI can reach. The Tauri shell keeps one `Arc<Nook>` as managed state.
pub struct Nook {
    pub home: Home,
    /// The key/value settings; shared, since the updater keeps its channel in them.
    pub settings: Arc<Settings>,
    /// The engines on the GPU, the model registry, the catalog and the Hub
    /// (`RuntimeConfig.runtimeManager`).
    pub runtime: Arc<RuntimeManager>,
    /// Settings › Models' downloads (`ModelDownloadService`).
    pub downloads: Arc<ModelDownloadService>,
    /// Self-update (`VersionUpdateService`).
    pub updater: Arc<Updater>,
    /// The Code worker's web access and its switch (`web/WebAccess`).
    pub web: Arc<WebAccess>,
    /// The voice prompt's microphone (`AudioRecorderService`).
    pub recorder: Arc<Recorder>,
    /// Code sessions and their turns (`CodeService`), on the runtime as its worker runtime and
    /// the web access; cheap to clone, every clone the same service.
    pub code: CodeService,
    /// The Video page's clip queue (`VideoStudio`), on the runtime as its video runtime.
    pub video: Arc<VideoStudio>,
    /// The Flows page's runs and the downloads they need (`FlowService`).
    pub flows: Arc<FlowService>,
    /// The PDF editor flow (PDFium on a thread of its own, started with the first PDF).
    pub pdf: Arc<PdfEditor>,
    /// The PDF editor's one download, its engine.
    pub pdf_setup: Arc<PdfInstaller>,
    /// The document converter Nooklet.
    pub convert: Arc<ConvertService>,
    /// The finder: picks the Nooklet for a request typed in a sentence.
    pub nooklets: Arc<Finder>,
    /// The screen recorder Nooklet.
    pub capture: Arc<CaptureService>,
    /// The loopback gateway while it runs (started in [`Nook::start`] on
    /// `gateway_port::choose()`, writing `home.gateway_file()`).
    gateway: Mutex<Option<GatewayHandle>>,
    /// Set by the first [`Nook::start`], so a second one starts nothing twice.
    started: AtomicBool,
    /// Cancelled at shutdown: stops work the UI started that the runtime does not own (the hub's
    /// engine install).
    stopping: CancellationToken,
    /// The updater's scheduled checks, stopped at shutdown.
    update_schedule: Mutex<Option<JoinHandle<()>>>,
}

impl Nook {
    /// Builds the services for a home. Does no network or engine work; [`Nook::start`] does.
    pub fn new(home: Home) -> Result<Arc<Nook>> {
        home.ensure_layout()?;
        // The first start after the Kotlin Nook: its sessions, worker files and engines come over
        // before anything reads this home (once; nothing when this home is in use already).
        crate::migrate::import_once(&home, crate::migrate::kotlin_home().as_deref());
        let settings = Arc::new(Settings::load(home.settings_file())?);
        let runtime = RuntimeManager::new(RuntimeConfig::new(home.clone())?);
        let downloads = ModelDownloadService::new(runtime.clone());
        let channels: Arc<dyn ChannelStore> = settings.clone();
        let updater = Updater::new(
            UpdateSource::from_env(Some(channels)),
            home.temp_dir().join("update"),
        );
        // An automatic (dev channel) install waits while either has a download running.
        updater.register_busy("RuntimeManager", runtime.clone());
        updater.register_busy("ModelDownloadService", downloads.clone());
        let web = Arc::new(WebAccess::new(&home).context("Could not set up web access")?);
        let recorder = Arc::new(Recorder::new(home.temp_dir()));
        // Reads the saved sessions: a run that was in flight when Nook closed is marked ended.
        let worker_runtime: Arc<dyn WorkerRuntime> = runtime.clone();
        let code = CodeService::new(home.clone(), worker_runtime, web.clone());
        let video = VideoStudio::for_runtime(runtime.clone());
        let flows = FlowService::for_runtime(runtime.clone());
        let packages = runtime.packages().clone();
        let pdf = Arc::new(PdfEditor::new(move || {
            // One build for every backend.
            packages
                .is_installed(EngineComponent::Pdfium, Backend::Cpu)
                .then(|| {
                    packages.executable(
                        EngineComponent::Pdfium,
                        Backend::Cpu,
                        EngineComponent::Pdfium.executables(),
                    )
                })
        }));
        // Nor while a Code turn works, a clip renders or a flow runs.
        updater.register_busy("CodeService", Arc::new(code.clone()));
        updater.register_busy("VideoStudio", video.clone());
        updater.register_busy("FlowService", flows.clone());
        // Nor while a PDF has changes that are not saved yet: an update restarts Nook.
        updater.register_busy("PdfEditor", pdf.clone());
        let pdf_setup = PdfInstaller::new(runtime.clone());
        updater.register_busy("PdfInstaller", pdf_setup.clone());
        let convert = ConvertService::new(runtime.clone(), pdf.clone(), home.clone());
        // Summarize and Read aloud read documents through the converter's engines.
        flows.set_reader(convert.clone());
        updater.register_busy("ConvertService", convert.clone());
        let nooklets = Finder::new(runtime.clone(), home.clone());
        updater.register_busy("Finder", nooklets.clone());
        let capture = CaptureService::new(runtime.clone(), settings.clone());
        updater.register_busy("CaptureService", capture.clone());
        Ok(Arc::new(Nook {
            home,
            settings,
            runtime,
            downloads,
            updater,
            web,
            recorder,
            code,
            video,
            flows,
            pdf,
            pdf_setup,
            convert,
            nooklets,
            capture,
            gateway: Mutex::new(None),
            started: AtomicBool::new(false),
            stopping: CancellationToken::new(),
            update_schedule: Mutex::new(None),
        }))
    }

    /// Background work after the window is up: the runtime picks its backend and starts its idle
    /// sweep and the resume of interrupted downloads; the loopback gateway starts (a port it
    /// cannot bind is logged: Nook works without its gateway); the updater starts its scheduled
    /// checks (the Hub asks for the first one itself once it is up). A second call does nothing.
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.runtime.start().await;
        if !self.stopping.is_cancelled() {
            match Gateway::start(self.runtime.clone(), self.video.clone(), &self.home).await {
                Ok(gateway) => {
                    // (GatewayInfo logs where it listens.)
                    *self.gateway.lock() = Some(gateway);
                    // Nook began closing while it bound: shutdown found no gateway to stop.
                    if self.stopping.is_cancelled() {
                        let late = self.gateway.lock().take();
                        if let Some(g) = late {
                            g.stop().await;
                        }
                    }
                }
                Err(e) => tracing::error!("{e:#}; Nook goes on without its gateway"),
            }
        }
        {
            let mut schedule = self.update_schedule.lock();
            if schedule.is_none() && !self.stopping.is_cancelled() {
                *schedule = Some(self.updater.start());
            }
        }
        Ok(())
    }

    /// A token cancelled at shutdown, for long work a command starts.
    pub fn stopping(&self) -> CancellationToken {
        self.stopping.child_token()
    }

    /// Stops engines and background work before exit: the gateway (no new requests, its event
    /// streams ended, `gateway.json` removed), the Code turns and the clip being rendered, then
    /// the engines.
    pub async fn shutdown(&self) {
        self.stopping.cancel();
        if let Some(schedule) = self.update_schedule.lock().take() {
            schedule.abort();
        }
        let recorder = self.recorder.clone();
        let _ = tokio::task::spawn_blocking(move || recorder.cancel()).await;
        let gateway = self.gateway.lock().take();
        if let Some(gateway) = gateway {
            gateway.stop().await;
        }
        self.code.shutdown();
        // A recording is kept: its file is finished before the engines stop.
        self.capture.shutdown().await;
        self.video.shutdown().await;
        self.flows.shutdown().await;
        self.pdf_setup.shutdown();
        self.convert.shutdown();
        self.nooklets.shutdown().await;
        self.runtime.shutdown().await;
    }
}
