/**
 * What the window shell asks of the runtime (nook_core::runtime, src-tauri/src/commands/runtime.rs):
 * the hub's first-start steps (HubScreen.kt) and the GPU load the brand mark spins with (FanMark.kt);
 * and, for Settings › Runtime and Models, RuntimeManager's status, its downloads, unload and pin,
 * and the GPU snapshot. The model catalog, the library's downloads and the Hub are in ./models.
 */
import { call, on } from "./ipc";

export interface RuntimeInit {
  /**
   * Models are installed but the engine is not (a backend switch, a deleted runtime): the hub then
   * installs it in the background. False on a first start with no model: the first model download
   * installs the runtime (RuntimeManager.download), not an overlay at start (QA of 2026-09-23).
   * `!runtime.isEngineInstalled && runtime.registry().list().isNotEmpty()`, false when that fails.
   */
  needsEngine: boolean;
  /** `runtime.backend().label()`: "NVIDIA CUDA 12", "Vulkan", "CPU". */
  backendLabel: string;
}

export const runtimeInit = () => call<RuntimeInit>("runtime_init");

/** `ModelDownloadService.refreshSync()`: reads the installed models and the catalog again. */
export const runtimeRefreshModels = () => call<void>("runtime_refresh_models");

/**
 * `RuntimeManager.ensureEngineInstalled`: downloads and installs the engine for the backend. True
 * when installed, false when the install stopped; rejects with the failure's message.
 */
export const runtimeEnsureEngine = () => call<boolean>("runtime_ensure_engine");

/** `RuntimeManager.gpuLoad()`: 0 while Nook leaves the GPU alone, up to 1 at full load. */
export const runtimeGpuLoad = () => call<number>("runtime_gpu_load");

// ------------------------------------------------------------------ status (Settings › Runtime)
//
// Types mirror the Java records of RuntimeManager (Status, EngineInfo, SpeechInfo, RuntimeEvent),
// SpeedProbe.Result and GpuInventory (GpuDevice, Metrics, Snapshot) field for field, camelCase.
// Enums are the Java constant names; instants ISO-8601 strings (chrono `DateTime<Utc>`).
//
// Events: every RuntimeManager event (RuntimeManager.emit, what its listeners got) is pushed on the
// topic "runtime" with the [RuntimeEvent] as payload; the pages re-read `runtime_status` on it.

/** backend.rs `Backend`: which engine build runs the models. */
export type Backend = "CUDA" | "VULKAN" | "CPU";

/** `Backend.label()`. */
export const BACKEND_LABELS: Record<Backend, string> = {
  CUDA: "NVIDIA CUDA 12",
  VULKAN: "Vulkan",
  CPU: "CPU",
};

/** `EngineProcess.State`. */
export type EngineState = "STARTING" | "READY" | "STOPPING" | "STOPPED" | "FAILED";

/** `RuntimeManager.Readiness`. */
export type Readiness = "ENGINE_MISSING" | "NO_MODELS" | "READY";

/** `RuntimeManager.RuntimeEvent`: "model_loaded", "download_failed", "engine_crashed", ... */
export interface RuntimeEvent {
  kind: string;
  modelId: string | null;
  detail: string | null;
  at: string;
}

/**
 * `RuntimeManager.EngineInfo`: one loaded text engine. `gpuLayers` is -1 for automatic, `layers`
 * 0 when the model's header is unknown; `ctxPerSlot` is the context one request sees.
 */
export interface EngineInfo {
  modelId: string;
  displayName: string;
  state: EngineState;
  port: number;
  gpuLayers: number;
  layers: number;
  ctxPerSlot: number;
  slots: number;
  inFlight: number;
  pinned: boolean;
  startedAt: string | null;
  lastUsed: string | null;
  failure: string | null;
  active: number;
  waitingInteractive: number;
  waitingBackground: number;
  tensorSplit: string | null;
}

/** `RuntimeManager.SpeechInfo`: one loaded whisper engine. */
export interface SpeechInfo {
  modelId: string;
  port: number;
  inFlight: number;
  lastUsed: string | null;
}

/** `SpeedProbe.MIN_WORKER_TPS`: below this a Nook Code turn takes longer than a person will wait for. */
export const MIN_WORKER_TPS = 8.0;

/**
 * `SpeedProbe.Result`: a model's measured speed on this machine (rounded to one decimal).
 * `fastEnoughToWork()` is `generateTps >= MIN_WORKER_TPS` ([fastEnoughToWork]).
 */
export interface SpeedProbeResult {
  modelId: string;
  sha256: string | null;
  /** The GPU driver version, or "cpu". */
  driver: string | null;
  backend: string | null;
  gpuLayers: number;
  promptTps: number;
  generateTps: number;
  measuredAt: string;
}

export const fastEnoughToWork = (p: SpeedProbeResult) => p.generateTps >= MIN_WORKER_TPS;

/** gpu_inventory.rs `GpuDevice` (GpuInventory.GpuDevice). `usedBytes()` is `totalBytes - freeBytes`. */
export interface GpuDevice {
  index: number;
  name: string;
  totalBytes: number;
  freeBytes: number;
  driverVersion: string | null;
  computeCapability: string | null;
  vendor: string;
  integrated: boolean;
}

/** gpu_inventory.rs `Metrics`: live load of one card; null where no sensor reports it. */
export interface GpuMetrics {
  index: number;
  usagePercent: number | null;
  temperatureC: number | null;
}

/** gpu_inventory.rs `Snapshot` (GpuInventory.Snapshot). */
export interface GpuSnapshot {
  devices: GpuDevice[];
  metrics: GpuMetrics[];
}

/** `RuntimeManager.Status`: the live view Settings › Runtime draws. */
export interface RuntimeStatus {
  backend: Backend;
  engineVersion: string;
  engineInstalled: boolean;
  speechEngineInstalled: boolean;
  imageEngineInstalled: boolean;
  devices: GpuDevice[];
  /** Free memory over every device less the driver reserve: what models may use. */
  budgetBytes: number;
  engines: EngineInfo[];
  speechEngines: SpeechInfo[];
  imageBusy: boolean;
  installedModels: string[];
  readiness: Readiness;
  speechEngineVersion: string;
  imageEngineVersion: string;
  /** RuntimeManager.downloads(): progress 0..1 by model id or [hubKey]. */
  downloads: Record<string, number>;
  /** Newest first, at most 40. */
  recentEvents: RuntimeEvent[];
  probes: SpeedProbeResult[];
}

/** `RuntimeManager.status()`. */
export const runtimeStatus = () => call<RuntimeStatus>("runtime_status");

/**
 * `RuntimeManager.downloads()`: progress (0..1) of the downloads the runtime runs itself: Hugging
 * Face variants under "hub:<repo>:<key>" and catalog models an agent asked for, by model id.
 */
export const runtimeDownloads = () => call<Record<string, number>>("runtime_downloads");

/** `RuntimeManager.unload(modelId)`: stops its text or speech engine now. */
export const runtimeUnload = (modelId: string) => call<void>("runtime_unload", { modelId });

/** `RuntimeManager.pin(modelId, pin)`: a pinned model is never evicted for another. */
export const runtimePin = (modelId: string, pin: boolean) => call<void>("runtime_pin", { modelId, pin });

/**
 * `RuntimeManager.load(modelId)`: loads a text model now (nothing when resident); rejects with the
 * admission message when it cannot be placed. No screen calls it (models load on first use, as in
 * the original); it is there for QA and the live checks.
 */
export const runtimeLoad = (modelId: string) => call<void>("runtime_load", { modelId });

/** `GpuInventory.snapshot()`: the devices and their load, cached for a couple of seconds. */
export const gpuSnapshot = () => call<GpuSnapshot>("gpu_snapshot");

/** Calls back with each runtime event ("runtime" topic). */
export const onRuntime = (fn: (event: RuntimeEvent) => void) => on<RuntimeEvent>("runtime", fn);
