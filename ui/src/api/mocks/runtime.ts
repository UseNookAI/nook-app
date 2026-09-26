import { mock, mockEmit } from "../ipc";
import type { EngineInfo, GpuDevice, RuntimeEvent, RuntimeStatus, SpeechInfo, SpeedProbeResult } from "../runtime";
import { mockFlag } from "./app";

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

const MIB = 1024 * 1024;
const MINUTE = 60_000;

/**
 * The runtime the Settings pages look at: an RTX 4060 (8 GB) with two text engines and a speech
 * engine loaded, three speed probes (one too slow for Code) and a feed of recent events. With
 * `?runtime=empty` nothing is loaded, measured or read yet (the pages' notes).
 */
const empty = () => mockFlag("runtime") === "empty";

const iso = (msAgo: number) => new Date(Date.now() - msAgo).toISOString();

function engine(over: Partial<EngineInfo> & Pick<EngineInfo, "modelId" | "displayName">): EngineInfo {
  return {
    state: "READY",
    port: 41500,
    gpuLayers: -1,
    layers: 0,
    ctxPerSlot: 8192,
    slots: 1,
    inFlight: 0,
    pinned: false,
    startedAt: iso(14 * MINUTE),
    lastUsed: iso(MINUTE),
    failure: null,
    active: 0,
    waitingInteractive: 0,
    waitingBackground: 0,
    tensorSplit: null,
    ...over,
  };
}

/** What each loaded engine holds on the card, in MiB (for the free memory). */
const ENGINE_MIB: Record<string, number> = { "gpt-oss-20b": 3_300, "qwen3-8b-q4km": 2_150, "whisper-small": 330 };

/**
 * State the models mock shares: the downloads RuntimeManager runs itself (Hugging Face variants
 * under "hub:<repo>:<key>"), the installed ids, and a way to add to the event feed.
 */
export const runtimeMock = {
  downloads: new Map<string, number>(),
  installedIds: (): string[] => [],
  engines: [] as EngineInfo[],
  speech: [] as SpeechInfo[],
  events: [] as RuntimeEvent[],
  /** RuntimeManager.emit: newest first, at most 200 kept, pushed on the "runtime" topic. */
  event(kind: string, modelId: string | null, detail: string | null): void {
    const e: RuntimeEvent = { kind, modelId, detail, at: new Date().toISOString() };
    this.events.unshift(e);
    this.events.length = Math.min(this.events.length, 200);
    mockEmit("runtime", e);
  },
  /** RuntimeManager.unload: stops the model's text or speech engine. */
  unload(modelId: string): void {
    const before = this.engines.length + this.speech.length;
    this.engines = this.engines.filter((e) => e.modelId !== modelId);
    this.speech = this.speech.filter((s) => s.modelId !== modelId);
    if (this.engines.length + this.speech.length < before) this.event("model_evicted", modelId, "unloaded");
  },
};

function seed(): void {
  if (empty()) return;
  runtimeMock.engines = [
    engine({
      modelId: "gpt-oss-20b",
      displayName: "gpt-oss 20B",
      port: 41501,
      gpuLayers: 99,
      layers: 24,
      inFlight: 1,
      pinned: true,
      active: 1,
      waitingBackground: 1,
    }),
    engine({ modelId: "qwen3-8b-q4km", displayName: "Qwen3 8B", port: 41502, gpuLayers: 20, layers: 36, slots: 2, ctxPerSlot: 4096 }),
  ];
  runtimeMock.speech = [{ modelId: "whisper-small", port: 41503, inFlight: 0, lastUsed: iso(6 * MINUTE) }];
  const at = (minutesAgo: number, kind: string, modelId: string | null, detail: string | null): RuntimeEvent => ({
    kind,
    modelId,
    detail,
    at: iso(minutesAgo * MINUTE),
  });
  runtimeMock.events = [
    at(1, "model_loaded", "qwen3-8b-q4km", "port=41502 slots=2 ctx=4096 per request"),
    at(1, "partial_offload", "qwen3-8b-q4km", "20 of 36 layers on GPU with 2 slots of 4096 (2650 MB free)"),
    at(2, "model_loading", "qwen3-8b-q4km", "ngl=20 ctx=4096 per request x 2"),
    at(6, "model_loaded", "whisper-small", "port=41503"),
    at(6, "model_loading", "whisper-small", "speech"),
    at(9, "engine_crashed", "qwen3-coder-30b-a3b", "CUDA error: out of memory\n  current device: 0, in function ggml_cuda_pool_malloc"),
    at(11, "download_failed", "sdxl-turbo", "the download was cut off (HTTP 503)"),
    at(13, "probe_measured", "gpt-oss-20b", "generate 31.4 tok/s, prompt 412.7 tok/s (driver 581.29)"),
    at(14, "model_loaded", "gpt-oss-20b", "port=41501 slots=1 ctx=8192 per request"),
    at(14, "experts_in_ram", "gpt-oss-20b", "32 experts per layer in system memory, attention on the GPU (24 GB of RAM free)"),
    at(15, "model_loading", "gpt-oss-20b", "ngl=99 ctx=8192 per request x 1"),
    at(16, "engine_installed", null, "llama b6153 (cuda)"),
    at(17, "engine_installing", "gpt-oss-20b", "llama"),
  ];
}

const probes = (): SpeedProbeResult[] =>
  empty()
    ? []
    : [
        { modelId: "gpt-oss-20b", sha256: "be37a636…", driver: "581.29", backend: "cuda", gpuLayers: 99, promptTps: 412.7, generateTps: 31.4, measuredAt: iso(13 * MINUTE) },
        { modelId: "qwen3-8b-q4km", sha256: "d98cdcbd…", driver: "581.29", backend: "cuda", gpuLayers: 36, promptTps: 1180.5, generateTps: 42, measuredAt: iso(3 * 24 * 60 * MINUTE) },
        { modelId: "qwen3-coder-30b-a3b", sha256: "7f1e3b44…", driver: "581.29", backend: "cuda", gpuLayers: 99, promptTps: 210.4, generateTps: 6.3, measuredAt: iso(26 * 60 * MINUTE) },
      ];

/** The card, its free memory following what is loaded (with a little jitter, as a live reading has). */
function devices(): GpuDevice[] {
  if (empty()) return [];
  const loaded = [...runtimeMock.engines, ...runtimeMock.speech].reduce((sum, e) => sum + (ENGINE_MIB[e.modelId] ?? 1_500), 0);
  const used = 610 + loaded + Math.round(Math.random() * 40);
  return [
    {
      index: 0,
      name: "NVIDIA GeForce RTX 4060",
      totalBytes: 8188 * MIB,
      freeBytes: Math.max(0, 8188 - used) * MIB,
      driverVersion: "581.29",
      computeCapability: "8.9",
      vendor: "nvidia",
      integrated: false,
    },
  ];
}

function status(): RuntimeStatus {
  const devs = devices();
  const budget = devs.reduce((sum, d) => sum + Math.max(0, d.freeBytes - 512 * MIB), 0);
  return {
    backend: "CUDA",
    engineVersion: "b6153",
    engineInstalled: true,
    speechEngineInstalled: !empty(),
    imageEngineInstalled: false,
    devices: devs,
    budgetBytes: budget,
    engines: runtimeMock.engines.map((e) => ({ ...e })),
    speechEngines: runtimeMock.speech.map((s) => ({ ...s })),
    imageBusy: false,
    installedModels: runtimeMock.installedIds(),
    readiness: "READY",
    speechEngineVersion: "v1.7.6",
    imageEngineVersion: "master-ab0d2d6",
    downloads: Object.fromEntries(runtimeMock.downloads),
    recentEvents: runtimeMock.events.slice(0, 40).map((e) => ({ ...e })),
    probes: probes(),
  };
}

export function registerRuntimeMocks(): void {
  const started = performance.now();
  seed();
  mock("runtime_init", async () => {
    await wait(500);
    return { needsEngine: mockFlag("engine") !== null, backendLabel: "NVIDIA CUDA 12" };
  });
  mock("runtime_refresh_models", () => wait(700));
  mock("runtime_ensure_engine", async () => {
    await wait(2500);
    if (mockFlag("engine") === "fail") throw new Error("the download was cut off (HTTP 503)");
    return true;
  });
  // With ?gpu the load swells and falls every twenty seconds, idle for a while in between.
  mock("runtime_gpu_load", () => {
    if (mockFlag("gpu") === null) return 0;
    const t = ((performance.now() - started) / 1000) % 20;
    return t < 12 ? 0.35 + 0.65 * Math.sin((t / 12) * Math.PI) : 0;
  });

  mock("runtime_status", async () => {
    await wait(120);
    return status();
  });
  mock("runtime_downloads", () => Object.fromEntries(runtimeMock.downloads));
  mock("runtime_unload", async ({ modelId }) => {
    await wait(300);
    runtimeMock.unload(String(modelId));
  });
  mock("runtime_pin", ({ modelId, pin }) => {
    runtimeMock.engines = runtimeMock.engines.map((e) => (e.modelId === modelId ? { ...e, pinned: pin === true } : e));
  });
  mock("runtime_load", async ({ modelId }) => {
    const id = String(modelId);
    if (runtimeMock.engines.some((e) => e.modelId === id)) return;
    if (!runtimeMock.installedIds().includes(id)) throw new Error(`Model not installed: ${id}`);
    runtimeMock.event("model_loading", id, "ngl=99 ctx=8192 per request x 1");
    await wait(1500);
    runtimeMock.engines = [...runtimeMock.engines, engine({ modelId: id, displayName: id, port: 41504 })];
    runtimeMock.event("model_loaded", id, "port=41504 slots=1 ctx=8192 per request");
  });
  mock("gpu_snapshot", () => ({ devices: devices(), metrics: empty() ? [] : [{ index: 0, usagePercent: 38, temperatureC: 61 }] }));
}
