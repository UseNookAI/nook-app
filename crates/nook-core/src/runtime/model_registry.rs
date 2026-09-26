//! Ports `runtime/ModelRegistry.java`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use regex::Regex;
use serde::{Serialize, Serializer};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::downloader::{part_path, with_suffix, Downloader, Outcome};
use super::engine_component::EngineComponent;
use super::gguf_metadata::GgufMetadata;
use super::model_catalog::{text, CatalogModel, ModelCatalog};
use super::Progress;
use crate::home::Home;

/// The workers.json key for the model that writes Nook Code's changes.
pub const CODE_WORKER: &str = "code";

/// The task of a file Nook cannot run (see [`LocalModel::unsupported`]): no task picks it, so it
/// is never a worker, a chat model or loaded, but it stays listed so it can be deleted.
pub const UNSUPPORTED: &str = "unsupported";

/// How long a scan stays fresh: downloads and deletes invalidate at once; external changes are
/// discovered within this.
const SCAN_TTL: Duration = Duration::from_secs(2);

/// A model on disk.
///
/// - `metadata`: the GGUF header of a language model, None for other formats (whisper ggml,
///   diffusion safetensors) and diffusion GGUFs; serialized as a summary.
/// - `shared`: found in the installed Nook's models folder ([`Home::shared_models_dir`]): usable
///   like any other, but never deleted or written by this app.
/// - `unsupported`: why Nook cannot run the file, for a GGUF from the Hub or dropped in by hand
///   whose header is not a language model's ([`GgufMetadata::not_a_language_model`]: a vision
///   encoder, a projector, prediction heads), whatever its sidecar says; its task is then
///   [`UNSUPPORTED`]. None for every model Nook can run, and for catalog models, which are
///   trusted as they are.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModel {
    pub id: String,
    pub display_name: String,
    pub family: String,
    pub task: String,
    pub file: PathBuf,
    pub bytes: u64,
    pub sha256: Option<String>,
    pub source: String,
    pub downloaded_at: Option<DateTime<Utc>>,
    #[serde(serialize_with = "metadata_summary")]
    pub metadata: Option<Arc<GgufMetadata>>,
    pub shared: bool,
    pub unsupported: Option<String>,
}

fn metadata_summary<S: Serializer>(
    m: &Option<Arc<GgufMetadata>>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    match m {
        Some(m) => m.as_ref().serialize(s),
        None => s.serialize_none(),
    }
}

impl LocalModel {
    pub fn is_chat(&self) -> bool {
        self.task == "chat"
    }
    pub fn is_embedding(&self) -> bool {
        self.task == "embed"
    }
    pub fn is_speech(&self) -> bool {
        self.task == "speech"
    }
    pub fn is_image(&self) -> bool {
        self.task == "image"
    }
    pub fn is_video(&self) -> bool {
        self.task == "video"
    }
    /// "X can't run in Nook: its architecture ... is ...", for a file Nook cannot run.
    pub fn unsupported_message(&self) -> Option<String> {
        self.unsupported
            .as_ref()
            .map(|why| format!("{} can't run in Nook: {why}.", self.display_name))
    }
    pub fn component(&self) -> EngineComponent {
        EngineComponent::for_task(&self.task)
    }
}

struct CachedFile {
    size: u64,
    modified: Option<SystemTime>,
    sidecar_modified: Option<SystemTime>,
    model: Option<LocalModel>,
}

struct ScanState {
    cache: HashMap<PathBuf, CachedFile>,
    snapshot: Vec<LocalModel>,
    next_scan: Option<Instant>,
}

/// Index of models present on disk. The filesystem is the source of truth: every model file under
/// the models directory with a `<file>.json` sidecar is a local model. Catalog downloads write the
/// sidecar; a bare GGUF dropped in by hand gets a sidecar generated from its metadata on the next
/// scan. Non-GGUF files (whisper ggml, diffusion safetensors) need a sidecar to be listed.
///
/// Models in the installed Nook's models folder are listed too, read-only: a bare GGUF there gets
/// its sidecar in memory only, and delete refuses them.
///
/// Listing is synchronous file work (a directory walk and, for new files, a GGUF header read);
/// it is cached for two seconds.
pub struct ModelRegistry {
    home: Home,
    shared_dir: Option<PathBuf>,
    catalog: Arc<ModelCatalog>,
    downloader: Arc<Downloader>,
    state: Mutex<ScanState>,
    prefs_lock: Mutex<()>,
}

impl ModelRegistry {
    /// A registry over this home's models and the installed Nook's shared models folder.
    pub fn new(
        home: Home,
        catalog: Arc<ModelCatalog>,
        downloader: Arc<Downloader>,
    ) -> ModelRegistry {
        let shared = home.shared_models_dir();
        ModelRegistry::with_shared_dir(home, catalog, downloader, shared)
    }

    /// A registry with an explicit read-only models folder beside the home's own (None: none).
    pub fn with_shared_dir(
        home: Home,
        catalog: Arc<ModelCatalog>,
        downloader: Arc<Downloader>,
        shared_dir: Option<PathBuf>,
    ) -> ModelRegistry {
        ModelRegistry {
            home,
            shared_dir,
            catalog,
            downloader,
            state: Mutex::new(ScanState {
                cache: HashMap::new(),
                snapshot: Vec::new(),
                next_scan: None,
            }),
            prefs_lock: Mutex::new(()),
        }
    }

    pub fn catalog(&self) -> &Arc<ModelCatalog> {
        &self.catalog
    }

    /// This app's models folder, where downloads go.
    pub fn models_dir(&self) -> PathBuf {
        self.home.models_dir()
    }

    /// The installed Nook's models folder this registry also reads, if any.
    pub fn shared_models_dir(&self) -> Option<&Path> {
        self.shared_dir.as_deref()
    }

    /// Downloads and deletes invalidate immediately; external changes are discovered within two
    /// seconds.
    pub fn invalidate(&self) {
        self.state.lock().next_scan = None;
    }

    /// Scans the models directories. Partial downloads (with a .part beside them) are excluded.
    /// This app's own models come first; a shared model with the same id as one of them is left
    /// out.
    pub fn list(&self) -> Vec<LocalModel> {
        let mut state = self.state.lock();
        if let Some(at) = state.next_scan {
            if Instant::now() < at {
                return state.snapshot.clone();
            }
        }
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let own = self.home.models_dir();
        if own.is_dir() {
            self.scan(&own, true, &mut state.cache, &mut seen, &mut out);
        }
        if let Some(shared) = self.shared_dir.as_ref().filter(|d| d.is_dir()) {
            let ids: HashSet<String> = out.iter().map(|m| m.id.clone()).collect();
            let mut shared_models = Vec::new();
            self.scan(
                shared,
                false,
                &mut state.cache,
                &mut seen,
                &mut shared_models,
            );
            out.extend(shared_models.into_iter().filter(|m| !ids.contains(&m.id)));
        }
        state.cache.retain(|p, _| seen.contains(p));
        state.snapshot = out.clone();
        state.next_scan = Some(Instant::now() + SCAN_TTL);
        out
    }

    fn scan(
        &self,
        root: &Path,
        own: bool,
        cache: &mut HashMap<PathBuf, CachedFile>,
        seen: &mut HashSet<PathBuf>,
        out: &mut Vec<LocalModel>,
    ) {
        // Folders linked into the models folder count as part of it: the original's Files.walk
        // went into directory junctions (Java reports them as directories), which is how a models
        // folder on another drive is usually shared. The depth bounds a link that loops.
        let mut files: Vec<PathBuf> = walkdir::WalkDir::new(root)
            .max_depth(3)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
            .map(|e| e.into_path())
            .filter(|p| is_model_file(p) && p.is_file())
            .collect();
        files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        for p in files {
            if part_path(&p).exists() {
                continue;
            }
            seen.insert(p.clone());
            let Ok(meta) = std::fs::metadata(&p) else {
                continue;
            };
            let size = meta.len();
            let modified = meta.modified().ok();
            let sidecar = sidecar_of(&p);
            let sidecar_modified = std::fs::metadata(&sidecar).and_then(|m| m.modified()).ok();
            let cached = cache.get(&p).filter(|c| {
                c.size == size && c.modified == modified && c.sidecar_modified == sidecar_modified
            });
            let model = match cached {
                Some(c) => c.model.clone(),
                None => match self.load(&p, own) {
                    Ok(m) => {
                        // A sidecar written just now counts as seen.
                        let sidecar_modified =
                            std::fs::metadata(&sidecar).and_then(|m| m.modified()).ok();
                        cache.insert(
                            p.clone(),
                            CachedFile {
                                size,
                                modified,
                                sidecar_modified,
                                model: m.clone(),
                            },
                        );
                        m
                    }
                    Err(e) => {
                        tracing::warn!("Skipping unreadable model {}: {e:#}", p.display());
                        continue;
                    }
                },
            };
            if let Some(m) = model {
                out.push(m);
            }
        }
    }

    pub fn find(&self, id: &str) -> Option<LocalModel> {
        self.list().into_iter().find(|m| m.id == id)
    }

    pub fn is_installed(&self, id: &str) -> bool {
        self.find(id).is_some()
    }

    /// The installed model that serves a task: the person's worker choice when set and installed,
    /// else the caller's preference (the catalog default), else the first installed one.
    pub fn first_for_task(&self, task: &str, preferred_id: Option<&str>) -> Option<LocalModel> {
        let all: Vec<LocalModel> = self.list().into_iter().filter(|m| m.task == task).collect();
        if let Some(chosen) = self.worker_preferences().get(task) {
            if let Some(pick) = all.iter().find(|m| &m.id == chosen) {
                return Some(pick.clone());
            }
        }
        if let Some(pref) = preferred_id {
            if let Some(m) = all.iter().find(|m| m.id == pref) {
                return Some(m.clone());
            }
        }
        all.into_iter().next()
    }

    // ------------------------------------------------------------------ worker preferences

    /// `<home>\runtime\workers.json`, where the original kept it under its home.
    pub fn workers_file(&self) -> PathBuf {
        self.home.runtime_dir().join("workers.json")
    }

    /// Task to model id: which installed model does the agents' work for chat, embed, speech and
    /// image; plus the `thinking`, `slots` and `ctx` switches.
    pub fn worker_preferences(&self) -> BTreeMap<String, String> {
        let _guard = self.prefs_lock.lock();
        self.read_preferences()
    }

    fn read_preferences(&self) -> BTreeMap<String, String> {
        let f = self.workers_file();
        let Ok(bytes) = std::fs::read(&f) else {
            return BTreeMap::new();
        };
        match serde_json::from_slice::<Value>(crate::settings::strip_bom(&bytes)) {
            Ok(Value::Object(map)) => map
                .iter()
                .filter(|(_, v)| !v.is_null() && !v.is_object() && !v.is_array())
                .filter_map(|(k, v)| text(Some(v)).map(|t| (k.clone(), t)))
                .collect(),
            Ok(_) => BTreeMap::new(),
            Err(e) => {
                tracing::warn!("Could not read {}: {e}", f.display());
                BTreeMap::new()
            }
        }
    }

    /// Sets (or with None or blank clears) the worker for a task.
    pub fn set_worker_preference(&self, task: &str, model_id: Option<&str>) -> Result<()> {
        let _guard = self.prefs_lock.lock();
        let mut prefs = self.read_preferences();
        match model_id {
            Some(id) if !id.trim().is_empty() => {
                prefs.insert(task.to_string(), id.to_string());
            }
            _ => {
                prefs.remove(task);
            }
        }
        let json = serde_json::to_vec_pretty(&prefs)?;
        crate::settings::write_atomic(&self.workers_file(), &json)
    }

    /// The model currently serving a task, after preferences and defaults.
    pub fn worker_for(&self, task: &str) -> Option<LocalModel> {
        let default = match task {
            "chat" => self.catalog.default_chat_model(),
            "speech" => self.catalog.default_speech_model(),
            "image" => self.catalog.default_image_model(),
            "video" => self.catalog.default_video_model(),
            "embed" => Some("nomic-embed-text-v1.5-q8"),
            _ => None,
        };
        self.first_for_task(task, default)
    }

    /// The installed model Nook Code's turns go to: the person's choice under [`CODE_WORKER`] in
    /// workers.json, else the first installed catalog worker (the default first). None when no
    /// worker is installed; the plain chat model is not a substitute, it is not made for tool use.
    /// A choice that is no chat model (a file Nook cannot run, chosen before it knew) counts as
    /// no choice, as one that is not installed does.
    pub fn code_worker(&self) -> Option<LocalModel> {
        if let Some(chosen) = self.worker_preferences().get(CODE_WORKER) {
            if let Some(pick) = self.find(chosen).filter(LocalModel::is_chat) {
                return Some(pick);
            }
        }
        self.catalog
            .worker_models()
            .into_iter()
            .find_map(|c| self.find(&c.id))
    }

    // ------------------------------------------------------------------ downloads

    /// True when a resumable partial download exists for the catalog model.
    pub fn has_partial(&self, id: &str) -> bool {
        self.catalog
            .find(id)
            .and_then(|m| self.target_for(m))
            .map(|t| part_path(&t).exists())
            .unwrap_or(false)
    }

    /// Where a catalog model's file goes: `models\<family>\<file>`.
    pub fn target_for(&self, model: &CatalogModel) -> Option<PathBuf> {
        model
            .primary_artifact()
            .map(|a| self.artifact_target(model, &a.file))
    }

    /// Where one of a catalog model's files goes: `models\<family>\<file>` (the resume check on
    /// start looks for a `.part` beside each).
    pub fn artifact_target(&self, model: &CatalogModel, file: &str) -> PathBuf {
        self.home.models_dir().join(&model.family).join(file)
    }

    /// Downloads a catalog model with resume and verification, then writes its sidecar.
    /// `progress` gets `(done, total)` over all of the model's files.
    ///
    /// Returns true when the model is installed afterwards, false when cancelled.
    pub async fn download(
        &self,
        id: &str,
        progress: Option<Progress>,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let model = self
            .catalog
            .find(id)
            .ok_or_else(|| anyhow!("Unknown catalog model {id}"))?
            .clone();
        let total = model.total_bytes();
        let mut before = 0u64;
        for (i, artifact) in model.artifacts.iter().enumerate() {
            let target = self.artifact_target(&model, &artifact.file);
            let offset = before;
            let per_file: Option<Progress> = progress.clone().map(|p| {
                let p: Progress = Arc::new(move |done, _| p(offset + done, total));
                p
            });
            let outcome = self
                .downloader
                .download(
                    &artifact.url,
                    &target,
                    artifact.sha256.as_deref(),
                    artifact.bytes,
                    per_file.as_ref(),
                    cancel,
                )
                .await?;
            if outcome == Outcome::Cancelled {
                return Ok(false);
            }
            before += tokio::fs::metadata(&target)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            write_catalog_sidecar(&target, &model, artifact.sha256.as_deref(), i == 0)?;
        }
        tracing::info!("Model {id} installed");
        self.invalidate();
        Ok(true)
    }

    /// Deletes a model and every file of it (a multi-file model's VAE and text encoder, a partial
    /// download). Returns whether anything was removed. A model found in the installed Nook's
    /// folder is refused: this app never deletes outside its own home.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let local = self.find(id);
        if let Some(m) = local.as_ref().filter(|m| m.shared) {
            let folder = self
                .shared_dir
                .as_deref()
                .map(|d| d.display().to_string())
                .unwrap_or_default();
            bail!(
                "{} is in the previous Nook's models folder ({folder}), which this Nook only reads. Delete it from there.",
                m.display_name
            );
        }
        let mut removed = false;
        if let Some(m) = &local {
            removed |= self.delete_quietly(&m.file);
            removed |= self.delete_quietly(&sidecar_of(&m.file));
        }
        // Every artifact of a multi-file model (VAE, text encoder) and any partial download go too.
        if let Some(m) = self.catalog.find(id) {
            for a in &m.artifacts {
                let target = self.artifact_target(m, &a.file);
                removed |= self.delete_quietly(&target);
                self.delete_quietly(&sidecar_of(&target));
                self.delete_quietly(&part_path(&target));
            }
        }
        self.invalidate();
        Ok(removed)
    }

    /// Deletes a file inside this app's models folder; anything elsewhere is left alone.
    fn delete_quietly(&self, p: &Path) -> bool {
        if !p.starts_with(self.home.models_dir()) {
            tracing::warn!(
                "Not deleting {}: it is outside this app's models folder",
                p.display()
            );
            return false;
        }
        match std::fs::remove_file(p) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                tracing::warn!("Could not delete {}: {e}", p.display());
                false
            }
        }
    }

    /// Reads one model file. `writable`: a bare GGUF gets its generated sidecar written (this
    /// app's folder) or kept in memory (the shared folder).
    fn load(&self, file: &Path, writable: bool) -> Result<Option<LocalModel>> {
        let sidecar = sidecar_of(file);
        let file_name = file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let gguf = file_name.to_lowercase().ends_with(".gguf");
        let mut meta: Option<Value> = if sidecar.exists() {
            let bytes = std::fs::read(&sidecar)
                .with_context(|| format!("Could not read {}", sidecar.display()))?;
            Some(
                serde_json::from_slice(&bytes)
                    .with_context(|| format!("{} is not valid JSON", sidecar.display()))?,
            )
        } else {
            None
        };
        if meta.is_none() && !gguf {
            return Ok(None); // a bare non-GGUF file: we cannot tell what it is
        }
        if meta.as_ref().and_then(|m| text(m.get("role"))).as_deref() == Some("component") {
            return Ok(None); // a VAE or text encoder that belongs to another model
        }
        // Only language models carry a header worth reading; a diffusion GGUF has none of those keys.
        let language_model = match &meta {
            None => true,
            Some(m) => matches!(
                text(m.get("task")).as_deref().unwrap_or("chat"),
                "chat" | "embed"
            ),
        };
        let gg = if gguf && language_model {
            Some(Arc::new(GgufMetadata::read(file)?))
        } else {
            None
        };
        let size = std::fs::metadata(file)?.len();
        let meta = match meta.take() {
            Some(m) => m,
            None => {
                // A bare GGUF: gguf && language_model, so the header was read.
                let gg = gg
                    .as_deref()
                    .ok_or_else(|| anyhow!("{} has no readable header", file.display()))?;
                let cat = self.catalog.find_by_file(&file_name);
                let n = json!({
                    "id": cat.map(|c| c.id.clone()).unwrap_or_else(|| slug(&file_name)),
                    "displayName": cat.map(|c| c.display_name.clone()).unwrap_or_else(|| gg.name()),
                    "family": cat.map(|c| c.family.clone()).unwrap_or_else(|| gg.architecture().to_string()),
                    "task": cat.map(|c| c.task.clone()).unwrap_or_else(|| if gg.is_embedding_model() { "embed" } else { "chat" }.to_string()),
                    "source": if cat.is_some() { "catalog" } else { "local" },
                    "downloadedAt": now_iso(),
                    "bytes": size,
                });
                if writable {
                    write_json(&sidecar, &n)?;
                }
                n
            }
        };
        let source = text(meta.get("source")).unwrap_or_else(|| "local".to_string());
        // A GGUF from the Hub or dropped in by hand is a language model only when its header
        // says so, whatever its sidecar claims: a vision encoder downloaded from the Hub was
        // written down as a chat model, became the Code worker and failed every load
        // (2026-09-26). Catalog models are known.
        let unsupported = gg
            .as_deref()
            .filter(|_| source != "catalog")
            .and_then(GgufMetadata::not_a_language_model);
        let task = match &unsupported {
            Some(_) => UNSUPPORTED.to_string(),
            None => text(meta.get("task")).unwrap_or_else(|| "chat".to_string()),
        };
        Ok(Some(LocalModel {
            id: text(meta.get("id")).unwrap_or_default(),
            display_name: text(meta.get("displayName")).unwrap_or_else(|| {
                gg.as_ref()
                    .map(|g| g.name())
                    .unwrap_or_else(|| file_name.clone())
            }),
            family: text(meta.get("family")).unwrap_or_else(|| {
                gg.as_ref()
                    .map(|g| g.architecture().to_string())
                    .unwrap_or_default()
            }),
            task,
            file: file.to_path_buf(),
            bytes: size,
            sha256: text(meta.get("sha256")),
            source,
            downloaded_at: text(meta.get("downloadedAt")).and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            }),
            metadata: gg,
            shared: !writable,
            unsupported,
        }))
    }
}

fn is_model_file(p: &Path) -> bool {
    let n = p
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    n.ends_with(".gguf") || n.ends_with(".bin") || n.ends_with(".safetensors")
}

/// `<file>.json` beside a model file.
pub fn sidecar_of(file: &Path) -> PathBuf {
    with_suffix(file, ".json")
}

/// An instant as Java's `Instant.toString()` wrote it (UTC, `Z`).
pub(crate) fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

pub(crate) fn write_json(path: &Path, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    std::fs::write(path, bytes).with_context(|| format!("Could not write {}", path.display()))
}

fn write_catalog_sidecar(
    file: &Path,
    model: &CatalogModel,
    sha256: Option<&str>,
    primary: bool,
) -> Result<()> {
    let size = std::fs::metadata(file)?.len();
    let n = json!({
        "id": model.id,
        "role": if primary { "model" } else { "component" },
        "displayName": model.display_name,
        "family": model.family,
        "task": model.task,
        "source": "catalog",
        "sha256": sha256,
        "bytes": size,
        "downloadedAt": now_iso(),
        "defaultCtx": model.default_ctx(),
    });
    write_json(&sidecar_of(file), &n)
}

static MODEL_EXT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\.(gguf|bin|safetensors)$").expect("extension pattern"));
static NON_ALNUM: Lazy<Regex> = Lazy::new(|| Regex::new(r"[^a-z0-9]+").expect("slug pattern"));

fn slug(file_name: &str) -> String {
    let base = MODEL_EXT.replace(file_name, "");
    NON_ALNUM
        .replace_all(&base.to_lowercase(), "-")
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::downloader::tests::serve;
    use crate::runtime::gguf_metadata::testing::{
        write_gguf, write_gguf_named, write_gguf_tensors, write_language_model, Kv,
    };
    use axum::routing::get;
    use axum::Router;
    use sha2::Digest;

    fn registry(home: &Home, shared: Option<PathBuf>) -> ModelRegistry {
        home.ensure_layout().unwrap();
        ModelRegistry::with_shared_dir(
            home.clone(),
            Arc::new(ModelCatalog::bundled().unwrap()),
            Arc::new(Downloader::new()),
            shared,
        )
    }

    fn touch_later(path: &Path, secs: u64) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn caches_headers_but_discovers_edits_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        let registry = registry(&home, None);
        let file = write_language_model(
            &home.models_dir(),
            "test.gguf",
            "llama",
            &[("general.name", Kv::Str("First"))],
            0,
        );
        let first = registry.list().into_iter().next().unwrap();
        assert_eq!(first.id, "test");
        assert_eq!(registry.find(&first.id).unwrap().file, first.file);
        registry.invalidate();
        let again = registry.list().into_iter().next().unwrap();
        assert!(
            Arc::ptr_eq(
                first.metadata.as_ref().unwrap(),
                again.metadata.as_ref().unwrap()
            ),
            "Unchanged headers are not parsed again"
        );

        write_language_model(
            &home.models_dir(),
            "test.gguf",
            "llama",
            &[("general.name", Kv::Str("Changed model"))],
            0,
        );
        touch_later(&file, 2);
        registry.invalidate();
        assert_eq!(
            registry.list()[0].metadata.as_ref().unwrap().name(),
            "Changed model"
        );

        let sidecar = sidecar_of(&file);
        std::fs::write(
            &sidecar,
            r#"{"id":"renamed","task":"chat","displayName":"Edited"}"#,
        )
        .unwrap();
        touch_later(&sidecar, 4);
        registry.invalidate();
        assert!(registry.find("renamed").is_some());
        assert!(registry.find(&first.id).is_none());

        assert!(registry.delete("renamed").unwrap());
        assert!(registry.list().is_empty());
    }

    /// The vision encoder downloaded from the Hub on 2026-09-26: its sidecar said chat, it was
    /// offered as the Code worker and failed every load. Its header says what it is.
    #[test]
    fn a_file_that_is_not_a_language_model_stays_listed_but_is_never_picked() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        let registry = registry(&home, None);
        let hub = home
            .models_dir()
            .join("hub")
            .join("antirez-deepseek-v4-gguf");
        let encoder = write_gguf_tensors(
            &hub,
            "DeepSeek-V4-Flash-Vision-Encoder.gguf",
            &[
                ("general.architecture", Kv::Str("deepseek4-vision")),
                ("general.name", Kv::Str("DeepSeek V4 Flash Vision Encoder")),
                ("deepseek4-vision.block_count", Kv::U32(32)),
                ("deepseek4-vision.embedding_length", Kv::U32(1024)),
            ],
            316,
            0,
        );
        let id = "deepseek-v4-deepseekv4flashvisionencodergguf";
        std::fs::write(
            sidecar_of(&encoder),
            json!({"id": id, "role": "model", "displayName": "deepseek-v4 DeepSeek-V4-Flash-Vision-Encoder.gguf",
                "family": "deepseek-v4-gguf", "task": "chat", "source": "huggingface",
                "repo": "antirez/deepseek-v4-gguf"})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            registry.workers_file(),
            json!({"code": id, "chat": id}).to_string(),
        )
        .unwrap();

        let m = registry
            .find(id)
            .expect("still listed, so it can be deleted");
        assert_eq!(m.task, UNSUPPORTED);
        assert!(!m.is_chat());
        assert_eq!(
            m.unsupported_message().as_deref(),
            Some("deepseek-v4 DeepSeek-V4-Flash-Vision-Encoder.gguf can't run in Nook: its architecture 'deepseek4-vision' is a vision encoder or projector, not a language model.")
        );
        assert_eq!(m.source, "huggingface");
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["task"], "unsupported");
        assert!(json["unsupported"]
            .as_str()
            .unwrap()
            .starts_with("its architecture 'deepseek4-vision'"));
        assert_eq!(json["metadata"]["architecture"], "deepseek4-vision");

        // workers.json naming it counts as naming nothing installed.
        assert!(registry.code_worker().is_none(), "no worker: Code says so");
        assert!(registry.worker_for("chat").is_none());
        let chat = write_language_model(&hub, "Small-Chat-Q4_K_M.gguf", "qwen3", &[], 0);
        std::fs::write(
            sidecar_of(&chat),
            r#"{"id":"small-chat","task":"chat","source":"huggingface"}"#,
        )
        .unwrap();
        // A catalog model is trusted as it is, whatever its header lacks.
        write_gguf_named(
            &home.models_dir().join("gpt-oss"),
            "gpt-oss-20b-MXFP4.gguf",
            &[("general.architecture", Kv::Str("gpt-oss"))],
            0,
        );
        registry.invalidate();
        assert_eq!(registry.worker_for("chat").unwrap().id, "small-chat");
        let worker = registry.code_worker().expect("the catalog worker");
        assert_eq!(worker.id, "gpt-oss-20b");
        assert!(worker.is_chat() && worker.unsupported.is_none());
        assert!(registry.find("small-chat").unwrap().unsupported.is_none());

        registry.invalidate();
        let again = registry.find(id).unwrap();
        assert!(
            Arc::ptr_eq(
                m.metadata.as_ref().unwrap(),
                again.metadata.as_ref().unwrap()
            ),
            "the header is read once while the file stays as it is"
        );
        assert!(registry.delete(id).unwrap());
        assert!(!encoder.exists() && !sidecar_of(&encoder).exists());
    }

    #[test]
    fn partial_files_are_hidden_after_rescan() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        let registry = registry(&home, None);
        let file = write_gguf(&home.models_dir(), &[("general.name", Kv::Str("Test"))], 0);
        assert_eq!(registry.list().len(), 1);
        std::fs::File::create(part_path(&file)).unwrap();
        registry.invalidate();
        assert!(registry.list().is_empty());
    }

    #[test]
    fn a_bare_gguf_gets_a_sidecar_and_catalog_names() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        let registry = registry(&home, None);
        let file = write_gguf_named(
            &home.models_dir().join("qwen3"),
            "Qwen3-8B-Q4_K_M.gguf",
            &[("general.architecture", Kv::Str("qwen3"))],
            0,
        );
        let m = registry
            .find("qwen3-8b-q4km")
            .expect("known to the catalog by its file name");
        assert_eq!(m.display_name, "Qwen3 8B");
        assert_eq!(m.source, "catalog");
        assert!(m.is_chat());
        assert!(!m.shared);
        assert!(m.downloaded_at.is_some());
        assert!(
            sidecar_of(&file).is_file(),
            "the generated sidecar is written in this app's folder"
        );

        // A whisper model needs a sidecar; one marked as a component is never listed alone.
        std::fs::write(home.models_dir().join("ggml-base.bin"), b"x").unwrap();
        std::fs::write(home.models_dir().join("vae.safetensors"), b"x").unwrap();
        std::fs::write(
            sidecar_of(&home.models_dir().join("vae.safetensors")),
            r#"{"id":"wan","role":"component","task":"video"}"#,
        )
        .unwrap();
        registry.invalidate();
        assert_eq!(registry.list().len(), 1);
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["displayName"], "Qwen3 8B");
        assert_eq!(json["metadata"]["architecture"], "qwen3");
        assert_eq!(json["shared"], false);
    }

    #[test]
    fn shared_models_are_listed_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        let registry = registry(&home, Some(shared.path().to_path_buf()));
        let bare = write_gguf_named(
            &shared.path().join("hub").join("x--y"),
            "Local-Thing-Q4_K_M.gguf",
            &[],
            0,
        );
        let whisper = shared.path().join("whisper").join("ggml-small.bin");
        std::fs::create_dir_all(whisper.parent().unwrap()).unwrap();
        std::fs::write(&whisper, b"ggml").unwrap();
        std::fs::write(sidecar_of(&whisper), r#"{"id":"whisper-small","task":"speech","displayName":"Whisper Small","source":"catalog","downloadedAt":"2026-09-20T10:00:00.123456Z"}"#).unwrap();

        let all = registry.list();
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(all.iter().all(|m| m.shared));
        assert!(
            !sidecar_of(&bare).exists(),
            "nothing is written into the installed Nook's folder"
        );
        let local = registry.find("local-thing-q4-k-m").unwrap();
        assert_eq!(local.source, "local");
        assert!(
            registry.is_installed("whisper-small"),
            "a shared model counts as installed"
        );
        assert_eq!(registry.worker_for("speech").unwrap().id, "whisper-small");
        assert!(registry
            .find("whisper-small")
            .unwrap()
            .downloaded_at
            .is_some());

        let err = registry.delete("whisper-small").unwrap_err().to_string();
        assert!(err.contains("which this Nook only reads"), "{err}");
        assert!(
            whisper.is_file() && sidecar_of(&whisper).is_file(),
            "refused: nothing was deleted"
        );

        // This app's own copy wins over the shared one with the same id.
        let own = home.models_dir().join("whisper").join("ggml-small.bin");
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();
        std::fs::write(&own, b"ggml").unwrap();
        std::fs::write(
            sidecar_of(&own),
            r#"{"id":"whisper-small","task":"speech"}"#,
        )
        .unwrap();
        registry.invalidate();
        let m = registry.find("whisper-small").unwrap();
        assert!(!m.shared);
        assert_eq!(registry.list().len(), 2);
        assert!(registry.delete("whisper-small").unwrap());
        assert!(!own.exists());
        assert!(whisper.is_file(), "the shared copy is untouched");
        assert!(
            registry.find("whisper-small").unwrap().shared,
            "and still usable"
        );
    }

    /// Links `link` to the folder `target`: a directory junction on Windows (no privilege needed),
    /// a symlink elsewhere. False when the system would not make one.
    fn link_dir(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .arg("/c")
                .arg("mklink")
                .arg("/J")
                .arg(link)
                .arg(target)
                .output()
                .is_ok_and(|o| o.status.success())
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    #[test]
    fn models_in_a_linked_folder_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        let registry = registry(&home, Some(shared.path().to_path_buf()));
        let whisper = elsewhere.path().join("ggml-small.bin");
        std::fs::write(&whisper, b"ggml").unwrap();
        std::fs::write(
            sidecar_of(&whisper),
            r#"{"id":"whisper-small","task":"speech","displayName":"Whisper Small"}"#,
        )
        .unwrap();
        if !link_dir(elsewhere.path(), &shared.path().join("whisper")) {
            eprintln!("skipped: could not link a folder here");
            return;
        }
        let m = registry
            .find("whisper-small")
            .expect("found through the link");
        assert!(m.shared);
        assert_eq!(
            m.file,
            shared.path().join("whisper").join("ggml-small.bin"),
            "named by its path through the link"
        );
    }

    #[test]
    fn worker_preferences_live_in_runtime_workers_json() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        let registry = registry(&home, None);
        assert!(registry.worker_preferences().is_empty());
        assert_eq!(
            registry.workers_file(),
            dir.path().join("runtime").join("workers.json")
        );
        registry
            .set_worker_preference(CODE_WORKER, Some("gpt-oss-20b"))
            .unwrap();
        registry
            .set_worker_preference("speech", Some("whisper-base"))
            .unwrap();
        registry
            .set_worker_preference("speech", Some("  "))
            .unwrap();
        let prefs = registry.worker_preferences();
        assert_eq!(
            prefs.get(CODE_WORKER).map(String::as_str),
            Some("gpt-oss-20b")
        );
        assert!(!prefs.contains_key("speech"));

        // Numbers and switches written by hand read as text.
        std::fs::write(
            registry.workers_file(),
            r#"{"thinking":"on","slots":2,"ctx":null,"x":{"a":1}}"#,
        )
        .unwrap();
        let prefs = registry.worker_preferences();
        assert_eq!(prefs.get("slots").map(String::as_str), Some("2"));
        assert_eq!(prefs.get("thinking").map(String::as_str), Some("on"));
        assert!(!prefs.contains_key("ctx") && !prefs.contains_key("x"));

        // No worker installed: the chat model is not a substitute.
        assert!(registry.code_worker().is_none());
    }

    #[test]
    fn the_task_pick_follows_preference_then_default_then_first() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        let registry = registry(&home, None);
        for (name, id) in [
            ("a.bin", "whisper-base"),
            ("b.bin", "whisper-small"),
            ("c.bin", "other"),
        ] {
            let f = home.models_dir().join(name);
            std::fs::write(&f, b"x").unwrap();
            std::fs::write(
                sidecar_of(&f),
                format!(r#"{{"id":"{id}","task":"speech"}}"#),
            )
            .unwrap();
        }
        assert_eq!(
            registry.worker_for("speech").unwrap().id,
            "whisper-small",
            "the catalog default"
        );
        assert_eq!(
            registry.first_for_task("speech", None).unwrap().id,
            "whisper-base",
            "the first by file name"
        );
        registry
            .set_worker_preference("speech", Some("other"))
            .unwrap();
        assert_eq!(registry.worker_for("speech").unwrap().id, "other");
        registry
            .set_worker_preference("speech", Some("not-installed"))
            .unwrap();
        assert_eq!(registry.worker_for("speech").unwrap().id, "whisper-small");
    }

    #[tokio::test]
    async fn downloads_a_catalog_model_with_its_components() {
        let main = b"main model bytes".to_vec();
        let vae = b"vae".to_vec();
        let (m, v) = (main.clone(), vae.clone());
        let base = serve(
            Router::new()
                .route(
                    "/main.gguf",
                    get(move || {
                        let m = m.clone();
                        async move { m }
                    }),
                )
                .route(
                    "/vae.safetensors",
                    get(move || {
                        let v = v.clone();
                        async move { v }
                    }),
                ),
        )
        .await;
        let catalog = ModelCatalog::from_json(&format!(
            r#"{{"defaultImageModel":"pic","models":[{{"id":"pic","displayName":"Pic","family":"pics","task":"image",
                "artifacts":[
                  {{"file":"main.gguf","url":"{base}/main.gguf","sha256":"{}","bytes":{}}},
                  {{"file":"vae.safetensors","url":"{base}/vae.safetensors","bytes":{},"format":"safetensors"}}]}}]}}"#,
            hex::encode(sha2::Sha256::digest(&main)),
            main.len(),
            vae.len()
        ))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let home = Home::at(dir.path());
        home.ensure_layout().unwrap();
        let registry = ModelRegistry::with_shared_dir(
            home.clone(),
            Arc::new(catalog),
            Arc::new(Downloader::new()),
            None,
        );
        assert!(!registry.has_partial("pic"));
        let last = Arc::new(Mutex::new((0u64, 0u64)));
        let l = last.clone();
        let progress: Progress = Arc::new(move |d, t| *l.lock() = (d, t));
        assert!(registry
            .download("pic", Some(progress), &CancellationToken::new())
            .await
            .unwrap());
        let total = (main.len() + vae.len()) as u64;
        assert_eq!(*last.lock(), (total, total));
        let all = registry.list();
        assert_eq!(
            all.len(),
            1,
            "the VAE is a component, listed with its model: {all:?}"
        );
        assert_eq!(all[0].id, "pic");
        assert_eq!(
            all[0].file,
            home.models_dir().join("pics").join("main.gguf")
        );
        assert!(
            all[0].metadata.is_none(),
            "a diffusion GGUF has no language-model header"
        );
        assert_eq!(registry.worker_for("image").unwrap().id, "pic");
        assert_eq!(
            registry
                .download("nope", None, &CancellationToken::new())
                .await
                .unwrap_err()
                .to_string(),
            "Unknown catalog model nope"
        );

        std::fs::write(
            part_path(&home.models_dir().join("pics").join("vae.safetensors")),
            b"v",
        )
        .unwrap();
        assert!(registry.delete("pic").unwrap());
        assert!(registry.list().is_empty());
        assert!(
            std::fs::read_dir(home.models_dir().join("pics"))
                .unwrap()
                .next()
                .is_none(),
            "every file went"
        );
    }

    #[test]
    fn slugs_are_lower_case_dashes() {
        assert_eq!(slug("Local-Thing-Q4_K_M.gguf"), "local-thing-q4-k-m");
        assert_eq!(slug("__weird__.BIN"), "weird");
    }
}
