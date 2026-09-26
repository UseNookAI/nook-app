/**
 * Models: the curated catalog, the models on disk and their downloads, the Hugging Face browser,
 * which installed model does each kind of work, and the worker's web access
 * (nook_core::runtime, nook_core::web; src-tauri/src/commands/models.rs).
 *
 * Types mirror the Rust structs field for field (camelCase, as the Java records were named):
 * `model_catalog.rs` (CatalogModel, Artifact), `model_registry.rs` (LocalModel),
 * `gguf_metadata.rs` (its serialized summary), `hugging_face_hub.rs` (Repo, HubFile, Variant, Fit),
 * and the state of `service/ModelDownloadService.kt` (DownloadsState). Instants are ISO-8601
 * strings (chrono `DateTime<Utc>`), byte counts plain numbers.
 *
 * Events: the topic "downloads" fires whenever a download starts, moves on (at most a few times a
 * second), pauses, stops or ends, and when a model is deleted: ModelDownloadService's state or
 * RuntimeManager.downloads() changed. Its payload is not used; the page re-reads `models_downloads`
 * (and `models_installed` when the set in flight changed) and `runtime_downloads`.
 */
import { call, on } from "./ipc";

// ------------------------------------------------------------------ catalog and installed models

/** model_catalog.rs `Artifact`: one file of a catalog model. `bytes` 0 when the catalog does not say. */
export interface Artifact {
  file: string;
  url: string;
  sha256: string | null;
  bytes: number;
  /** "gguf" (the default), "ggml", "safetensors". */
  format: string;
}

/** model_catalog.rs `CatalogModel` (ModelCatalog.CatalogModel). Tasks: chat, embed, speech, image, video. */
export interface CatalogModel {
  id: string;
  displayName: string;
  family: string;
  task: string;
  paramsB: number;
  description: string;
  /** "tools", "json_schema", "thinking", "worker", "embeddings", "transcription", ... */
  capabilities: string[];
  artifacts: Artifact[];
  defaults: Record<string, string>;
  backends: Record<string, string>;
  license: string;
  minVramGb: number;
}

/**
 * The curated catalog as the UI reads it (ModelCatalog): every model and the default per task
 * (`defaultChatModel()` and the others), null where the catalog names none.
 */
export interface Catalog {
  models: CatalogModel[];
  defaultChatModel: string | null;
  defaultWorkerModel: string | null;
  defaultSpeechModel: string | null;
  defaultImageModel: string | null;
  defaultVideoModel: string | null;
}

/** gguf_metadata.rs: the summary a GgufMetadata serializes as. */
export interface GgufSummary {
  architecture: string;
  name: string;
  fileBytes: number;
  layers: number;
  contextLength: number;
  expertCount: number;
  mixtureOfExperts: boolean;
  embeddingModel: boolean;
}

/** model_registry.rs `UNSUPPORTED`: the task of a file Nook cannot run (see `LocalModel.unsupported`). */
export const UNSUPPORTED = "unsupported";

/**
 * model_registry.rs `LocalModel` (ModelRegistry.LocalModel): a model on disk. `shared` is one found
 * in the installed Nook's models folder: usable like any other, never deleted by this app.
 */
export interface LocalModel {
  id: string;
  displayName: string;
  family: string;
  /** chat, embed, speech, image, video, or `UNSUPPORTED`. */
  task: string;
  /** Absolute path of the model file. */
  file: string;
  bytes: number;
  sha256: string | null;
  /** "catalog", "huggingface", "local". */
  source: string;
  downloadedAt: string | null;
  /** The GGUF header of a language model; null for other formats. */
  metadata: GgufSummary | null;
  shared: boolean;
  /**
   * Why Nook cannot run the file, as a clause ("its architecture 'clip' is a vision encoder or
   * projector, not a language model"): a GGUF from the Hub or dropped in by hand whose header is no
   * language model's. Its task is then `UNSUPPORTED`; null for every model Nook can run.
   */
  unsupported: string | null;
}

/**
 * What ModelDownloadService.kt keeps about the library's downloads, one read of its state fields.
 * Keys are the catalog model id (`AiModelDto.uniqueKey`). A paused download keeps its last
 * progress; a stopping one is being cancelled and its files deleted.
 */
export interface DownloadsState {
  /** A refresh of the catalog and the installed list is running (`isLoading`). */
  isLoading: boolean;
  downloadingModels: string[];
  pausedModels: string[];
  stoppingModels: string[];
  deletingModels: string[];
  /** 0..1 by key, while downloading or paused. */
  downloadingProgress: Record<string, number>;
  /** Catalog (text-engine) downloads running now: more than one makes Cancel ask first. */
  textDownloadingCount: number;
}

export const EMPTY_DOWNLOADS: DownloadsState = {
  isLoading: false,
  downloadingModels: [],
  pausedModels: [],
  stoppingModels: [],
  deletingModels: [],
  downloadingProgress: {},
  textDownloadingCount: 0,
};

/** The catalog with its defaults (RuntimeManager.catalog(): ModelCatalog.all() and the default ids). */
export const modelsCatalog = () => call<Catalog>("models_catalog");

/**
 * The installed models (ModelDownloadService.installedModels): ModelRegistry.list() without the
 * ones still in flight, as refreshSync() keeps it. Shared models from the installed Nook included.
 */
export const modelsInstalled = () => call<LocalModel[]>("models_installed");

/** ModelDownloadService's download state (the fields of [DownloadsState]). */
export const modelsDownloads = () => call<DownloadsState>("models_downloads");

/** Starts downloading a catalog model, its engine first when missing (ModelDownloadService.launchDownload). */
export const modelsDownload = (id: string) => call<void>("models_download", { id });

/** Pauses a download; the partial file and its progress stay (ModelDownloadService.pauseDownload). */
export const modelsPause = (id: string) => call<void>("models_pause", { id });

/** Resumes a paused download, or starts it again when its job is gone (ModelDownloadService.resumeDownload). */
export const modelsResume = (id: string) => call<void>("models_resume", { id });

/** Stops a download and deletes what it wrote (ModelDownloadService.stopDownload). */
export const modelsCancel = (id: string) => call<void>("models_cancel", { id });

/**
 * Deletes an installed model, unloading it first (ModelDownloadService.deleteModel, via
 * NookAiService.deleteModel: RuntimeManager.unload + ModelRegistry.delete). Rejects for a shared
 * model with ModelRegistry.delete's message.
 */
export const modelsDelete = (id: string) => call<void>("models_delete", { id });

/** Calls back whenever a download moves on or the installed list changes ("downloads" events). */
export const onDownloads = (fn: () => void) => on<unknown>("downloads", fn);

// ------------------------------------------------------------------ Hugging Face

/** hugging_face_hub.rs `Repo` (HuggingFaceHub.Repo): a repository as the search lists it. */
export interface Repo {
  /** "unsloth/Qwen3-8B-GGUF". */
  id: string;
  author: string;
  name: string;
  downloads: number;
  likes: number;
  lastModified: string | null;
  gated: boolean;
  pipelineTag: string | null;
  tags: string[];
}

/** hugging_face_hub.rs `HubFile`: one GGUF file; shards of a split model share a base name. */
export interface HubFile {
  path: string;
  bytes: number;
  sha256: string | null;
  quant: string;
  shardIndex: number;
  shardCount: number;
  shardBase: string;
}

/**
 * hugging_face_hub.rs `Variant`: one downloadable quantisation, possibly in several shards. `key`
 * (the file name without shard suffix) is unique within a repository even when two model files
 * share a `label`.
 */
export interface Variant {
  label: string;
  key: string;
  files: HubFile[];
  totalBytes: number;
}

/** hugging_face_hub.rs `Fit` (HuggingFaceHub.Fit): whether a model runs comfortably on the largest GPU. */
export type Fit = "FITS" | "TIGHT" | "OFFLOAD" | "NO_GPU";

/** GGUF repositories matching the text, the most downloaded first; popular ones for "" (HuggingFaceHub.search). */
export const hubSearch = (query: string, limit = 30) => call<Repo[]>("hub_search", { query, limit });

/** The complete GGUF quantisations of a repository with their sizes (HuggingFaceHub.variants). */
export const hubVariants = (repoId: string) => call<Variant[]>("hub_variants", { repoId });

/** The keys of the variants already in the models folder (HuggingFaceHub.isInstalled for each). */
export const hubInstalled = (repoId: string, variants: Variant[]) => call<string[]>("hub_installed", { repoId, variants });

/**
 * Downloads a variant in the background, the text engine first when missing
 * (RuntimeManager.downloadHubAsync). False when that variant is already downloading. Progress shows
 * in `runtime_downloads` under [hubKey].
 */
export const hubDownload = (repo: Repo, variant: Variant) => call<boolean>("hub_download", { repo, variant });

/** Stops a hub download; the partial file stays so the next attempt resumes (RuntimeManager.cancelHubDownload). */
export const hubCancel = (repoId: string, variantKey: string) => call<void>("hub_cancel", { repoId, variantKey });

/** RuntimeManager.hubKey: the key a hub download reports its progress under. */
export const hubKey = (repoId: string, variantKey: string) => `hub:${repoId}:${variantKey}`;

// ------------------------------------------------------------------ workers

/** model_registry.rs CODE_WORKER: the workers.json key for the model that writes Nook Code's changes. */
export const CODE_WORKER = "code";

/** Task to chosen model id, plus the `thinking`, `slots` and `ctx` switches (ModelRegistry.workerPreferences). */
export const workersPreferences = () => call<Record<string, string>>("workers_preferences");

/**
 * The model serving each task now, after preferences and defaults: "code" is
 * ModelRegistry.codeWorker(), every other task ModelRegistry.workerFor(task). Tasks with none are
 * left out.
 */
export const workersCurrent = () => call<Record<string, string>>("workers_current");

/** Remembers (or with null clears) the model for a task in workers.json (ModelRegistry.setWorkerPreference). */
export const workersSet = (task: string, modelId: string | null) => call<void>("workers_set", { task, modelId });

// ------------------------------------------------------------------ web access

/** Whether Nook Code's worker may search the web and read pages (WebAccess.enabled). */
export const webAccessEnabled = () => call<boolean>("web_access_enabled");

/** Saves the switch in web.json in the Nook home; the next request uses it (WebAccess.setEnabled). */
export const webAccessSet = (enabled: boolean) => call<void>("web_access_set", { enabled });
