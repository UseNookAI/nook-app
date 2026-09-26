//! Ports `kotlin/.../service/ModelDownloadService.kt`, the download service behind Settings ›
//! Models (download, pause, resume, stop, delete, what is installed), with the parts of
//! `service/NookAiService.java` it calls (the model library as `AiModelDto`s, the installed
//! models as `PromptModel`s, download and delete) and the download lines of
//! `ui/component/popup/settings/ModelDownloadsStrip.kt`.
//!
//! The Kotlin service kept Compose state; here the state is a [`DownloadState`] snapshot
//! ([`ModelDownloadService::state`]) and every change is published on [`topic::DOWNLOADS`] as
//! `{"source":"library", ...DownloadState}` (progress at most four times a second). The runtime's
//! own downloads (Hub files, catalog models an agent asked for) come on the same topic as
//! `{"source":"runtime","downloads":{...}}`; [`download_lines`] merges both for the Models title.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::manager::RuntimeManager;
use super::model_catalog::CatalogModel;
use super::model_registry::{LocalModel, UNSUPPORTED};
use super::Progress;
use crate::busy::BusyWork;
use crate::events::{self, topic};

/// The one registry the library knows (`AiModelRegistry.NOOK`).
pub const NOOK_REGISTRY: &str = "NOOK";
/// How often download progress goes to the UI.
const PROGRESS_UI_EVERY: Duration = Duration::from_millis(250);

/// A model the library lists (`common/dto/AiModelDto.java`): a catalog model or a file placed in
/// the models folder by hand. `model` is its id and the download key.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiModelDto {
    pub model: String,
    pub full_name: Option<String>,
    pub description: Option<String>,
    pub tasks: Vec<String>,
    #[serde(rename = "requiredVramInGB")]
    pub required_vram_in_gb: Option<i64>,
    pub model_registry: Option<String>,
    #[serde(rename = "sizeInGB")]
    pub size_in_gb: Option<f64>,
    pub r#type: Option<String>,
    pub context_tokens: Option<u32>,
}

impl AiModelDto {
    /// The key downloads are tracked under (`AiModelDto.uniqueKey`).
    pub fn unique_key(&self) -> &str {
        &self.model
    }
}

/// An installed model (`model/PromptModel.java`); `use_case` carries the catalog task so the UI
/// can split chat, image and speech models.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptModel {
    pub name: String,
    pub modified_at: String,
    pub digest: Option<String>,
    pub use_case: Option<String>,
    #[serde(rename = "sizeInGB")]
    pub size_in_gb: Option<f32>,
    pub short_description: Option<String>,
    pub ai_model_registry: String,
}

/// What the Models page shows, named as the Kotlin service's state. The sets keep the order
/// things were added in.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadState {
    pub available_models: Vec<AiModelDto>,
    pub installed_models: Vec<PromptModel>,
    pub installed_model_names: Vec<String>,
    pub is_loading: bool,
    pub downloading_models: Vec<String>,
    pub text_downloading_count: u32,
    pub paused_models: Vec<String>,
    pub downloading_progress: BTreeMap<String, f64>,
    pub deleting_models: Vec<String>,
    pub stopping_models: Vec<String>,
}

#[derive(Serialize)]
struct LibraryDownloads<'a> {
    source: &'static str,
    #[serde(flatten)]
    state: &'a DownloadState,
}

/// One download in flight, as the Models title shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadLine {
    pub name: String,
    pub progress: f64,
    pub paused: bool,
}

/// The downloads in flight, the library's first (the download service runs those), then the ones
/// the runtime runs itself: Hugging Face files, keyed `"hub:<repo>:<file>"`, and catalog models an
/// agent asked for. A model both know of is listed once.
pub fn download_lines(
    library: &[AiModelDto],
    downloading: &[String],
    paused: &[String],
    progress: &BTreeMap<String, f64>,
    runtime: &BTreeMap<String, f64>,
    catalog_name: impl Fn(&str) -> Option<String>,
) -> Vec<DownloadLine> {
    let names: HashMap<&str, &str> = library
        .iter()
        .map(|m| {
            (
                m.model.as_str(),
                m.full_name.as_deref().unwrap_or(m.model.as_str()),
            )
        })
        .collect();
    let mut mine: Vec<&String> = Vec::new();
    for key in downloading.iter().chain(paused) {
        if !mine.contains(&key) {
            mine.push(key);
        }
    }
    let mut lines: Vec<DownloadLine> = mine
        .iter()
        .map(|key| DownloadLine {
            name: names.get(key.as_str()).copied().unwrap_or(key).to_string(),
            progress: progress.get(*key).copied().unwrap_or(0.0),
            paused: paused.contains(key),
        })
        .collect();
    for (key, p) in runtime.iter().filter(|(k, _)| !mine.contains(k)) {
        let name = if key.starts_with("hub:") {
            key.rsplit(':').next().unwrap_or(key).to_string()
        } else {
            catalog_name(key)
                .or_else(|| names.get(key.as_str()).map(|s| s.to_string()))
                .unwrap_or_else(|| key.clone())
        };
        lines.push(DownloadLine {
            name,
            progress: *p,
            paused: false,
        });
    }
    lines
}

/// UI task tag and type for a catalog task (chat, embed, speech, image, video), and for a file
/// Nook cannot run ([`UNSUPPORTED`]), which has no kind.
pub fn ui_task(task: &str) -> &'static str {
    match task {
        "embed" => "embedding",
        "speech" => "speech-to-text",
        "image" => "image-generation",
        "video" => "video-generation",
        UNSUPPORTED => UNSUPPORTED,
        _ => "text-generation",
    }
}

/// A catalog model as the library lists it.
pub fn catalog_dto(m: &CatalogModel) -> AiModelDto {
    let mut tasks = vec![ui_task(&m.task).to_string()];
    tasks.extend(m.capabilities.iter().cloned());
    AiModelDto {
        model: m.id.clone(),
        full_name: Some(m.display_name.clone()),
        description: Some(format!(
            "{}{}",
            m.description,
            if m.license.trim().is_empty() {
                String::new()
            } else {
                format!(" Licence: {}.", m.license)
            }
        )),
        tasks,
        required_vram_in_gb: Some(m.min_vram_gb),
        model_registry: Some(NOOK_REGISTRY.to_string()),
        size_in_gb: Some((m.size_gb() * 10.0).round() / 10.0),
        r#type: Some(ui_task(&m.task).to_string()),
        context_tokens: Some(m.default_ctx()),
    }
}

/// A model placed in the models folder by hand, as the library lists it. A file Nook cannot run
/// says why and needs no memory.
pub fn local_dto(m: &LocalModel) -> AiModelDto {
    let file = m.file.file_name().unwrap_or_default().to_string_lossy();
    let description = match (&m.unsupported, &m.metadata) {
        (Some(why), _) => format!("Nook can't run it: {why}. Added from {file}."),
        (None, Some(meta)) => format!(
            "Added from {file} ({}, {} layers, context {}).",
            meta.architecture(),
            meta.layers(),
            meta.context_length()
        ),
        (None, None) => format!("Added from {file} ({} model).", m.task),
    };
    AiModelDto {
        model: m.id.clone(),
        full_name: Some(m.display_name.clone()),
        description: Some(description),
        tasks: vec![ui_task(&m.task).to_string(), "local-file".to_string()],
        required_vram_in_gb: m
            .unsupported
            .is_none()
            .then(|| (m.bytes as f64 / 1e9).ceil() as i64 + 1),
        model_registry: Some(NOOK_REGISTRY.to_string()),
        size_in_gb: Some((m.bytes as f64 / 1e8).round() / 10.0),
        r#type: Some(ui_task(&m.task).to_string()),
        context_tokens: Some(m.metadata.as_ref().map(|g| g.context_length()).unwrap_or(0)),
    }
}

/// Everything the user can download: text and embedding models from the bundled catalog, plus
/// any model files placed in the models folder by hand (`NookAiService.getAllAvailableModels`).
pub fn all_available_models(runtime: &RuntimeManager) -> Vec<AiModelDto> {
    let mut all = Vec::new();
    let mut seen = HashSet::new();
    for m in runtime.catalog().all() {
        all.push(catalog_dto(m));
        seen.insert(m.id.clone());
    }
    // Models the user placed in the models folder by hand are still shown and manageable.
    for local in runtime.registry().list() {
        if seen.insert(local.id.clone()) {
            all.push(local_dto(&local));
        }
    }
    all
}

/// Models present on this machine (`NookAiService.listDownloadedModels`).
pub fn list_downloaded_models(runtime: &RuntimeManager) -> Vec<PromptModel> {
    runtime
        .registry()
        .list()
        .into_iter()
        .map(|m| PromptModel {
            name: m.id.clone(),
            modified_at: m
                .downloaded_at
                .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
                .unwrap_or_default(),
            digest: m.sha256.clone(),
            use_case: Some(m.task.clone()),
            size_in_gb: Some((m.bytes as f64 / 1e9) as f32),
            short_description: Some(m.display_name.clone()),
            ai_model_registry: NOOK_REGISTRY.to_string(),
        })
        .collect()
}

/// Whether an installed name belongs to a download in flight: exact, `:latest`, or a tag of it.
fn is_in_flight(installed_name: &str, in_flight_keys: &[String]) -> bool {
    in_flight_keys.iter().any(|key| {
        installed_name == key
            || installed_name == format!("{key}:latest")
            || installed_name.starts_with(&format!("{key}:"))
            || key
                .strip_suffix(":latest")
                .is_some_and(|base| installed_name == base)
    })
}

fn add(set: &mut Vec<String>, key: &str) {
    if !set.iter().any(|k| k == key) {
        set.push(key.to_string());
    }
}

fn remove(set: &mut Vec<String>, key: &str) {
    set.retain(|k| k != key);
}

/// Pauses a download (`PauseGate`): pausing stops the running attempt, whose partial file stays
/// for the resume.
struct PauseGate {
    paused: bool,
    attempt: CancellationToken,
}

struct Inner {
    state: DownloadState,
    jobs: HashMap<String, JoinHandle<()>>,
    gates: HashMap<String, Arc<Mutex<PauseGate>>>,
    stopped: HashSet<String>,
    last_progress_event: Option<Instant>,
}

/// The download service (`ModelDownloadService`). Share it as `Arc` (built with
/// [`ModelDownloadService::new`]); the app keeps one for its whole life.
pub struct ModelDownloadService {
    runtime: Arc<RuntimeManager>,
    inner: Mutex<Inner>,
    me: Weak<ModelDownloadService>,
}

impl ModelDownloadService {
    pub fn new(runtime: Arc<RuntimeManager>) -> Arc<ModelDownloadService> {
        Arc::new_cyclic(|me| ModelDownloadService {
            runtime,
            inner: Mutex::new(Inner {
                state: DownloadState::default(),
                jobs: HashMap::new(),
                gates: HashMap::new(),
                stopped: HashSet::new(),
                last_progress_event: None,
            }),
            me: me.clone(),
        })
    }

    /// The state the Models page shows.
    pub fn state(&self) -> DownloadState {
        self.inner.lock().state.clone()
    }

    /// The Models title's download lines: this service's downloads, then the runtime's own.
    pub fn download_lines(&self) -> Vec<DownloadLine> {
        let state = self.state();
        let catalog = self.runtime.catalog().clone();
        download_lines(
            &state.available_models,
            &state.downloading_models,
            &state.paused_models,
            &state.downloading_progress,
            &self.runtime.downloads(),
            |id| catalog.find(id).map(|m| m.display_name.clone()),
        )
    }

    /// Changes the state and publishes it.
    fn update(&self, change: impl FnOnce(&mut DownloadState)) {
        let mut inner = self.inner.lock();
        change(&mut inner.state);
        inner.last_progress_event = Some(Instant::now());
        publish(&inner.state);
    }

    fn spawn(
        &self,
        work: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Option<JoinHandle<()>> {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => Some(handle.spawn(work)),
            Err(_) => {
                tracing::warn!("The download service needs the app's async runtime");
                None
            }
        }
    }

    pub fn refresh_if_empty(&self) {
        let empty = {
            let inner = self.inner.lock();
            inner.state.available_models.is_empty() && !inner.state.is_loading
        };
        if empty {
            self.refresh();
        }
    }

    pub fn refresh(&self) {
        if let Some(me) = self.me.upgrade() {
            self.spawn(async move { me.refresh_sync().await });
        }
    }

    /// Re-reads the library and the installed list and writes them into the state. Models still
    /// downloading, paused or being stopped are not counted as installed.
    pub async fn refresh_sync(&self) {
        self.update(|s| s.is_loading = true);
        let runtime = self.runtime.clone();
        let read = tokio::task::spawn_blocking(move || {
            (
                all_available_models(&runtime),
                list_downloaded_models(&runtime),
            )
        })
        .await;
        match read {
            Ok((models, installed_raw)) => self.update(|s| {
                let mut in_flight = s.downloading_models.clone();
                in_flight.extend(s.paused_models.iter().cloned());
                in_flight.extend(s.stopping_models.iter().cloned());
                let installed: Vec<PromptModel> = installed_raw
                    .into_iter()
                    .filter(|m| !is_in_flight(&m.name, &in_flight))
                    .collect();
                s.available_models = models;
                s.installed_model_names = installed.iter().map(|m| m.name.clone()).collect();
                s.installed_models = installed;
            }),
            Err(e) => {
                tracing::error!("Failed to refresh model catalog and installed models: {e}")
            }
        }
        self.update(|s| s.is_loading = false);
    }

    // ------------------------------------------------------------------ download

    pub fn launch_download(&self, model: &AiModelDto) {
        let Some(me) = self.me.upgrade() else { return };
        let key = model.unique_key().to_string();
        let gate = Arc::new(Mutex::new(PauseGate {
            paused: false,
            attempt: CancellationToken::new(),
        }));
        self.inner.lock().gates.insert(key.clone(), gate.clone());
        let model = model.clone();
        let k = key.clone();
        let job = self.spawn(async move { me.run_download(model, k, gate).await });
        if let Some(job) = job {
            self.inner.lock().jobs.insert(key, job);
        }
    }

    async fn run_download(
        self: Arc<Self>,
        model: AiModelDto,
        key: String,
        gate: Arc<Mutex<PauseGate>>,
    ) {
        self.update(|s| {
            add(&mut s.downloading_models, &key);
            remove(&mut s.paused_models, &key);
        });
        self.inner.lock().stopped.remove(&key);
        let nook = model.model_registry.as_deref() == Some(NOOK_REGISTRY);
        if nook {
            self.update(|s| s.text_downloading_count += 1);
        } else {
            tracing::warn!(
                "Unsupported registry: {}",
                model.model_registry.as_deref().unwrap_or("null")
            );
        }
        loop {
            let attempt = if nook {
                let attempt = {
                    let mut g = gate.lock();
                    g.attempt = CancellationToken::new();
                    g.attempt.clone()
                };
                self.download_model(&model.model, &key, &attempt).await;
                Some(attempt)
            } else {
                None
            };
            // Decided under the same lock as the ending, so a resume is never lost.
            let mut inner = self.inner.lock();
            let stopped = inner.stopped.contains(&key);
            let paused = gate.lock().paused;
            // A resume that came while the pause was still stopping the download: go on.
            if attempt.is_some_and(|a| a.is_cancelled()) && !paused && !stopped {
                continue;
            }
            if nook {
                inner.state.text_downloading_count =
                    inner.state.text_downloading_count.saturating_sub(1);
            }
            let s = &mut inner.state;
            if stopped {
                // stopping clears everything
            } else if paused {
                add(&mut s.paused_models, &key);
                remove(&mut s.downloading_models, &key);
                // the last percent stays for the UI
            } else {
                remove(&mut s.downloading_models, &key);
                s.downloading_progress.remove(&key);
            }
            inner.jobs.remove(&key);
            if inner.gates.get(&key).is_some_and(|g| Arc::ptr_eq(g, &gate)) {
                inner.gates.remove(&key);
            }
            publish(&inner.state);
            break;
        }
        // The installed list changes: the Models page and the hub read it.
        self.refresh_sync().await;
    }

    /// Downloads a runtime model with progress (`NookAiService.downloadModel`); true when it is
    /// installed afterwards, false when cancelled or failed (the failure is logged).
    async fn download_model(&self, model_id: &str, key: &str, cancel: &CancellationToken) -> bool {
        let me = self.me.clone();
        let key = key.to_string();
        let progress: Progress = Arc::new(move |done, total| {
            if total == 0 {
                return;
            }
            if let Some(me) = me.upgrade() {
                me.progress(&key, (done as f64 / total as f64).clamp(0.0, 1.0));
            }
        });
        match self
            .runtime
            .download(model_id, Some(progress), cancel)
            .await
        {
            Ok(ok) => ok,
            Err(e) => {
                tracing::error!("Error downloading model {model_id}: {e:#}");
                false
            }
        }
    }

    fn progress(&self, key: &str, p: f64) {
        let mut inner = self.inner.lock();
        inner.state.downloading_progress.insert(key.to_string(), p);
        let due = inner
            .last_progress_event
            .is_none_or(|t| t.elapsed() >= PROGRESS_UI_EVERY);
        if due {
            inner.last_progress_event = Some(Instant::now());
            publish(&inner.state);
        }
    }

    // ------------------------------------------------------------------ pause, resume, stop

    pub fn pause_download(&self, key: &str) {
        let gate = self.inner.lock().gates.get(key).cloned();
        if let Some(gate) = gate {
            let mut g = gate.lock();
            g.paused = true;
            g.attempt.cancel();
        }
        self.update(|s| {
            add(&mut s.paused_models, key);
            remove(&mut s.downloading_models, key);
        });
    }

    pub fn resume_download(&self, model: &AiModelDto) {
        let key = model.unique_key();
        let resumed = {
            let inner = self.inner.lock();
            match inner.gates.get(key) {
                Some(gate) if gate.lock().paused => {
                    gate.lock().paused = false;
                    true
                }
                _ => false,
            }
        };
        if resumed {
            self.update(|s| {
                remove(&mut s.paused_models, key);
                add(&mut s.downloading_models, key);
            });
        } else {
            self.update(|s| remove(&mut s.paused_models, key));
            self.launch_download(model);
        }
    }

    /// Stops a download and deletes what it had downloaded.
    pub fn stop_download(&self, model: &AiModelDto) {
        let key = model.unique_key().to_string();
        let job = {
            let mut inner = self.inner.lock();
            if let Some(gate) = inner.gates.get(&key) {
                let mut g = gate.lock();
                g.paused = false;
                g.attempt.cancel();
            }
            inner.stopped.insert(key.clone());
            inner.jobs.remove(&key)
        };
        self.update(|s| {
            add(&mut s.stopping_models, &key);
            remove(&mut s.paused_models, &key);
            remove(&mut s.downloading_models, &key);
            s.downloading_progress.remove(&key);
        });
        let Some(me) = self.me.upgrade() else { return };
        let id = model.model.clone();
        let nook = model.model_registry.as_deref() == Some(NOOK_REGISTRY);
        self.spawn(async move {
            // The download lets go of its partial file first, so the delete can remove it.
            if let Some(job) = job {
                let _ = job.await;
            }
            if nook {
                me.delete_quietly(&id).await;
            }
            me.inner.lock().stopped.remove(&key);
            me.update(|s| remove(&mut s.stopping_models, &key));
            // A download that finished just before the stop was listed as installed: re-read.
            me.refresh_sync().await;
        });
    }

    /// Deletes an installed model (unloading it first); the installed list is re-read after.
    pub fn delete_model(&self, model: &AiModelDto) {
        let Some(me) = self.me.upgrade() else { return };
        let model = model.clone();
        self.spawn(async move {
            let key = model.unique_key().to_string();
            me.update(|s| add(&mut s.deleting_models, &key));
            if model.model_registry.as_deref() == Some(NOOK_REGISTRY) {
                me.delete_quietly(&model.model).await;
            } else {
                tracing::warn!(
                    "Unsupported registry: {}",
                    model.model_registry.as_deref().unwrap_or("null")
                );
            }
            me.update(|s| remove(&mut s.deleting_models, &key));
            me.refresh_sync().await;
        });
    }

    /// Removes a runtime model from disk, unloading it first if it is resident
    /// (`NookAiService.deleteModel`); a failure is logged.
    async fn delete_quietly(&self, model_id: &str) -> bool {
        self.runtime.unload(model_id).await;
        let registry = self.runtime.registry().clone();
        let id = model_id.to_string();
        match tokio::task::spawn_blocking(move || registry.delete(&id)).await {
            Ok(Ok(removed)) => removed,
            Ok(Err(e)) => {
                tracing::error!("Error deleting model {model_id}: {e:#}");
                false
            }
            Err(e) => {
                tracing::error!("Error deleting model {model_id}: {e}");
                false
            }
        }
    }
}

fn publish(state: &DownloadState) {
    events::emit(
        topic::DOWNLOADS,
        LibraryDownloads {
            source: "library",
            state,
        },
    );
}

impl BusyWork for ModelDownloadService {
    fn busy_with(&self) -> Option<String> {
        if self.inner.lock().state.downloading_models.is_empty() {
            None
        } else {
            Some("a model is downloading".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, name: &str) -> AiModelDto {
        AiModelDto {
            model: id.into(),
            full_name: Some(name.into()),
            ..Default::default()
        }
    }

    fn keys(k: &[&str]) -> Vec<String> {
        k.iter().map(|s| s.to_string()).collect()
    }

    fn map(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn library_downloads_show_their_name_and_hub_files_their_file() {
        let lines = download_lines(
            &[model("qwen3-coder-30b", "Qwen3 Coder 30B")],
            &keys(&["qwen3-coder-30b"]),
            &[],
            &map(&[("qwen3-coder-30b", 0.42)]),
            &map(&[("hub:unsloth/Qwen3-8B-GGUF:Qwen3-8B-Q4_K_M", 0.1)]),
            |_| None,
        );
        assert_eq!(
            lines,
            [
                DownloadLine {
                    name: "Qwen3 Coder 30B".into(),
                    progress: 0.42,
                    paused: false
                },
                DownloadLine {
                    name: "Qwen3-8B-Q4_K_M".into(),
                    progress: 0.1,
                    paused: false
                },
            ]
        );
    }

    #[test]
    fn a_paused_download_keeps_its_percent_and_one_model_is_listed_once() {
        let lines = download_lines(
            &[model("gpt-oss-20b", "gpt-oss 20B")],
            &[],
            &keys(&["gpt-oss-20b"]),
            &map(&[("gpt-oss-20b", 0.7)]),
            &map(&[("gpt-oss-20b", 0.7), ("whisper-small", 0.3)]),
            |id| (id == "whisper-small").then(|| "Whisper Small".to_string()),
        );
        assert_eq!(
            lines,
            [
                DownloadLine {
                    name: "gpt-oss 20B".into(),
                    progress: 0.7,
                    paused: true
                },
                DownloadLine {
                    name: "Whisper Small".into(),
                    progress: 0.3,
                    paused: false
                },
            ]
        );
    }

    #[test]
    fn nothing_in_flight_is_no_lines() {
        assert!(
            download_lines(&[], &[], &[], &BTreeMap::new(), &BTreeMap::new(), |_| None).is_empty()
        );
    }

    #[test]
    fn in_flight_names_match_their_tags() {
        let k = keys(&["qwen3", "llama:latest"]);
        assert!(is_in_flight("qwen3", &k));
        assert!(is_in_flight("qwen3:latest", &k));
        assert!(is_in_flight("qwen3:8b", &k));
        assert!(is_in_flight("llama", &k));
        assert!(!is_in_flight("qwen3-8b", &k));
        assert!(!is_in_flight("qwen3", &[]));
    }

    #[test]
    fn catalog_models_read_as_the_library_lists_them() {
        let catalog = crate::runtime::ModelCatalog::bundled().unwrap();
        let qwen = catalog_dto(catalog.find("qwen3-8b-q4km").unwrap());
        assert_eq!(qwen.full_name.as_deref(), Some("Qwen3 8B"));
        assert_eq!(qwen.tasks[0], "text-generation");
        assert_eq!(qwen.r#type.as_deref(), Some("text-generation"));
        assert_eq!(qwen.model_registry.as_deref(), Some("NOOK"));
        assert_eq!(qwen.required_vram_in_gb, Some(6));
        assert_eq!(qwen.context_tokens, Some(8192));
        let json = serde_json::to_value(&qwen).unwrap();
        assert!(json.get("requiredVramInGB").is_some() && json.get("sizeInGB").is_some());
        assert_eq!(json["type"], "text-generation");
        let whisper = catalog_dto(catalog.find("whisper-small").unwrap());
        assert_eq!(whisper.r#type.as_deref(), Some("speech-to-text"));
        assert_eq!(ui_task("video"), "video-generation");
        assert_eq!(ui_task("embed"), "embedding");
        assert_eq!(ui_task("image"), "image-generation");
    }

    #[test]
    fn a_file_nook_cannot_run_is_listed_as_one() {
        let m = LocalModel {
            id: "encoder".into(),
            display_name: "Vision Encoder".into(),
            family: "x".into(),
            task: UNSUPPORTED.into(),
            file: std::path::PathBuf::from("models").join("hub").join("Enc.gguf"),
            bytes: 932_857_760,
            sha256: None,
            source: "huggingface".into(),
            downloaded_at: None,
            metadata: None,
            shared: false,
            unsupported: Some("its architecture 'deepseek4-vision' is a vision encoder or projector, not a language model".into()),
        };
        let dto = local_dto(&m);
        assert_eq!(dto.tasks, ["unsupported", "local-file"]);
        assert_eq!(dto.r#type.as_deref(), Some("unsupported"));
        assert_eq!(dto.required_vram_in_gb, None);
        assert_eq!(
            dto.description.as_deref(),
            Some("Nook can't run it: its architecture 'deepseek4-vision' is a vision encoder or projector, not a language model. Added from Enc.gguf.")
        );
    }

    use crate::runtime::engine_component::EngineComponent;
    use crate::runtime::manager::testing::{install, rig, serve_files, wait_for, Rig, RigSpec};
    use std::time::Duration;

    /// A service over a runtime whose catalog has one speech model served slowly (about half a
    /// second), with the speech engine already installed.
    async fn service() -> (Rig, Arc<ModelDownloadService>, AiModelDto) {
        let weights: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
        let base = serve_files(
            vec![("/tiny.bin".into(), weights.clone())],
            Duration::from_millis(30),
        )
        .await;
        let catalog = format!(
            r#"{{"defaultSpeechModel":"tiny-whisper","models":[{{"id":"tiny-whisper","displayName":"Tiny Whisper",
                "family":"whisper","task":"speech","license":"MIT",
                "artifacts":[{{"file":"ggml-tiny.bin","url":"{base}/tiny.bin","bytes":{}}}]}}]}}"#,
            weights.len()
        );
        let rig = rig(RigSpec {
            catalog: Some(catalog),
            ..RigSpec::default()
        });
        install(&rig, EngineComponent::Whisper);
        let svc = ModelDownloadService::new(rig.manager.clone());
        svc.refresh_sync().await;
        let dto = svc
            .state()
            .available_models
            .into_iter()
            .find(|m| m.model == "tiny-whisper")
            .unwrap();
        (rig, svc, dto)
    }

    fn part(rig: &Rig) -> std::path::PathBuf {
        rig.home
            .models_dir()
            .join("whisper")
            .join("ggml-tiny.bin.part")
    }

    #[tokio::test]
    async fn a_download_runs_to_the_installed_list() {
        let (_rig, svc, dto) = service().await;
        assert_eq!(dto.full_name.as_deref(), Some("Tiny Whisper"));
        assert_eq!(dto.description.as_deref(), Some(" Licence: MIT."));
        assert_eq!(dto.r#type.as_deref(), Some("speech-to-text"));
        assert!(svc.state().installed_models.is_empty());
        assert_eq!(svc.busy_with(), None);

        svc.launch_download(&dto);
        wait_for("the download to start", || {
            svc.state().downloading_models == ["tiny-whisper"]
        })
        .await;
        assert_eq!(svc.busy_with().as_deref(), Some("a model is downloading"));
        assert_eq!(svc.state().text_downloading_count, 1);
        wait_for("progress", || {
            svc.state()
                .downloading_progress
                .get("tiny-whisper")
                .is_some_and(|p| *p > 0.0)
        })
        .await;
        let lines = svc.download_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].name, "Tiny Whisper");

        wait_for("the installed list", || {
            svc.state().installed_model_names == ["tiny-whisper"]
        })
        .await;
        let state = svc.state();
        assert!(state.downloading_models.is_empty());
        assert!(state.downloading_progress.is_empty());
        assert_eq!(state.text_downloading_count, 0);
        assert_eq!(
            state.installed_models[0].use_case.as_deref(),
            Some("speech")
        );
        assert_eq!(state.installed_models[0].ai_model_registry, "NOOK");
        assert_eq!(svc.busy_with(), None);
    }

    #[tokio::test]
    async fn a_paused_download_keeps_its_percent_and_resumes() {
        let (rig, svc, dto) = service().await;
        svc.launch_download(&dto);
        wait_for("progress", || {
            svc.state()
                .downloading_progress
                .get("tiny-whisper")
                .is_some_and(|p| *p > 0.0)
        })
        .await;
        svc.pause_download("tiny-whisper");
        assert_eq!(svc.state().paused_models, ["tiny-whisper"]);
        assert!(svc.state().downloading_models.is_empty());
        wait_for("the download to let go", || {
            svc.inner.lock().jobs.is_empty() && !svc.state().is_loading
        })
        .await;
        let state = svc.state();
        assert_eq!(state.paused_models, ["tiny-whisper"]);
        assert!(
            state.downloading_progress["tiny-whisper"] > 0.0,
            "the percent stays"
        );
        assert!(state.installed_model_names.is_empty());
        assert!(part(&rig).exists(), "the partial file stays for the resume");
        assert_eq!(svc.busy_with(), None, "paused is not busy");
        let lines = svc.download_lines();
        assert!(lines[0].paused);

        svc.resume_download(&dto);
        wait_for("the installed list", || {
            svc.state().installed_model_names == ["tiny-whisper"]
        })
        .await;
        assert!(svc.state().paused_models.is_empty());
        assert!(!part(&rig).exists());
    }

    #[tokio::test]
    async fn a_stopped_download_is_deleted_and_an_installed_model_can_be() {
        let (rig, svc, dto) = service().await;
        svc.launch_download(&dto);
        wait_for("progress", || {
            svc.state()
                .downloading_progress
                .get("tiny-whisper")
                .is_some_and(|p| *p > 0.0)
        })
        .await;
        svc.stop_download(&dto);
        let state = svc.state();
        assert_eq!(state.stopping_models, ["tiny-whisper"]);
        assert!(state.downloading_models.is_empty() && state.downloading_progress.is_empty());
        wait_for("the stop", || svc.state().stopping_models.is_empty()).await;
        assert!(!part(&rig).exists(), "what it had downloaded is gone");
        assert!(svc.state().installed_model_names.is_empty());
        assert!(svc.state().paused_models.is_empty());

        svc.launch_download(&dto);
        wait_for("the installed list", || {
            svc.state().installed_model_names == ["tiny-whisper"]
        })
        .await;
        svc.delete_model(&dto);
        wait_for("the delete", || {
            let s = svc.state();
            s.installed_model_names.is_empty() && s.deleting_models.is_empty()
        })
        .await;
        assert!(!rig.manager.registry().is_installed("tiny-whisper"));
    }

    #[tokio::test]
    async fn refreshing_an_empty_library_fills_it() {
        let rig = rig(RigSpec::default());
        let svc = ModelDownloadService::new(rig.manager.clone());
        assert!(svc.state().available_models.is_empty());
        svc.refresh_if_empty();
        wait_for("the library", || !svc.state().available_models.is_empty()).await;
        let state = svc.state();
        assert!(state
            .available_models
            .iter()
            .any(|m| m.model == "qwen3-8b-q4km"));
        assert!(!state.is_loading);
    }
}
