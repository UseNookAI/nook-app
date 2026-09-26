/**
 * Browser stand-ins for the model commands (ModelDownloadService, ModelRegistry, HuggingFaceHub,
 * WebAccess): the bundled catalog plus one model too big for an 8 GB card; installed models, two of
 * them shared from the installed Nook and one a vision encoder Nook cannot run; one download
 * running on a timer and one paused; a Hugging Face search with variants, one repository gated and
 * one with split files; and the worker preferences and web access switch.
 *
 * Address-bar switches: `models=none` (nothing installed or downloading), `hub=fail` (the search
 * cannot reach Hugging Face).
 */
import type { CodeSnapshot } from "../code";
import { call, mock, mockEmit } from "../ipc";
import type { Artifact, Catalog, CatalogModel, DownloadsState, LocalModel, Repo, Variant } from "../models";
import { hubKey, UNSUPPORTED } from "../models";
import { mockFlag } from "./app";
import { runtimeMock } from "./runtime";

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
const MODELS = "C:\\Users\\you\\AppData\\Local\\Nook-rs\\models";
const SHARED = "C:\\Users\\you\\AppData\\Local\\Nook\\models";

// ------------------------------------------------------------------ the catalog

const art = (file: string, bytes: number, format = "gguf"): Artifact => ({ file, url: `https://huggingface.co/mock/resolve/main/${file}`, sha256: null, bytes, format });

function entry(
  id: string,
  displayName: string,
  task: string,
  paramsB: number,
  capabilities: string[],
  minVramGb: number,
  license: string,
  description: string,
  artifacts: Artifact[],
  defaults: Record<string, string> = task === "chat" ? { nCtx: "8192", temperature: "0.7" } : {},
): CatalogModel {
  return {
    id,
    displayName,
    family: id.split("-")[0],
    task,
    paramsB,
    description,
    capabilities,
    artifacts,
    defaults,
    backends: { cuda: "verified", vulkan: "verified", cpu: "slow" },
    license,
    minVramGb,
  };
}

const CATALOG: CatalogModel[] = [
  entry("qwen3-4b-q4km", "Qwen3 4B", "chat", 4, ["tools", "json_schema", "thinking"], 4, "Apache-2.0",
    "Fast general assistant for 6 GB cards. Good at writing, summarising and everyday questions.", [art("Qwen3-4B-Q4_K_M.gguf", 2_497_281_632)]),
  entry("qwen3-8b-q4km", "Qwen3 8B", "chat", 8.2, ["tools", "json_schema", "thinking"], 6, "Apache-2.0",
    "The default chat model. Strong reasoning, coding and tool use on an 8 GB card.", [art("Qwen3-8B-Q4_K_M.gguf", 5_027_784_096)]),
  entry("llama-3.1-8b-instruct-q4km", "Llama 3.1 8B Instruct", "chat", 8, ["tools", "json_schema"], 6, "Llama-3.1",
    "Meta's 8B instruction model. A dependable all-rounder with tool calling.", [art("Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf", 4_920_734_656)]),
  entry("qwen3-32b-q4km", "Qwen3 32B", "chat", 32.8, ["tools", "json_schema", "thinking"], 20, "Apache-2.0",
    "The largest dense Qwen3. Near the best local reasoning, but it wants a 24 GB card to run at speed.", [art("Qwen3-32B-Q4_K_M.gguf", 19_762_149_024)]),
  entry("nomic-embed-text-v1.5-q8", "Nomic Embed Text v1.5", "embed", 0.14, ["embeddings"], 1, "Apache-2.0",
    "Small embedding model for search and retrieval.", [art("nomic-embed-text-v1.5.Q8_0.gguf", 146_000_000)], { nCtx: "2048" }),
  entry("whisper-small", "Whisper Small", "speech", 0.24, ["transcription", "multilingual"], 1, "MIT",
    "Speech to text for the voice prompt. Multilingual, good accuracy, about a second per sentence on a GPU.", [art("ggml-small.bin", 487_601_967, "ggml")]),
  entry("whisper-base", "Whisper Base", "speech", 0.07, ["transcription", "multilingual"], 0, "MIT",
    "Smaller, faster speech to text. Fine for short English prompts on any machine.", [art("ggml-base.bin", 147_951_465, "ggml")]),
  entry("sd-turbo", "SD Turbo", "image", 0.86, ["text-to-image"], 4, "stabilityai-community",
    "The default image model. 512 by 512 pictures in one to four steps; fits a 6 GB card.", [art("sd_turbo.safetensors", 5_214_561_328, "safetensors")]),
  entry("sdxl-turbo", "SDXL Turbo", "image", 3.5, ["text-to-image"], 8, "stabilityai-community",
    "Higher quality 512 by 512 pictures in one to four steps. Needs an 8 GB card; runs at 8-bit weights.", [art("sd_xl_turbo_1.0_fp16.safetensors", 6_938_081_905, "safetensors")]),
  entry("z-image-turbo", "Z-Image Turbo", "image", 6, ["text-to-image"], 6, "apache-2.0",
    "The best local picture quality: sharp 1024 by 1024 images in 8 steps. Three files; the text encoder runs from system memory so it fits an 8 GB card.",
    [art("z_image_turbo-Q4_K.gguf", 3_864_250_304), art("ae.safetensors", 335_304_388, "safetensors"), art("Qwen3-4B-Instruct-2507-Q4_K_M.gguf", 2_497_281_120)]),
  entry("wan2.1-t2v-1.3b", "Wan 2.1 T2V 1.3B", "video", 1.3, ["text-to-video"], 8, "apache-2.0",
    "Short video clips from a prompt, 832 by 480 at 16 frames a second. Three files; the text encoder runs on the processor and the rest on the GPU, so it fits an 8 GB card. A 2-second clip takes about 5 minutes on an RTX 4060.",
    [art("Wan2.1-T2V-1.3B-Q8_0.gguf", 1_535_768_800), art("umt5-xxl-encoder-Q5_K_M.gguf", 4_145_878_880), art("wan_2.1_vae.safetensors", 253_815_318, "safetensors")]),
  entry("gpt-oss-20b", "gpt-oss 20B", "chat", 21, ["tools", "worker", "thinking"], 8, "Apache-2.0",
    "OpenAI's open-weight mixture of experts, 3.6B active. Nook Code's default worker on an 8 GB card: with the experts in system RAM a change takes about three minutes. Needs 24 GB of RAM beside a small card.",
    [art("gpt-oss-20b-MXFP4.gguf", 12_109_566_624)], { nCtx: "8192", temperature: "0.2", minRamGb: "24", reasoningEffort: "low" }),
  entry("qwen3-coder-30b-a3b", "Qwen3-Coder 30B-A3B", "chat", 30.5, ["tools", "worker"], 8, "Apache-2.0",
    "The coding-tuned mixture of experts, 3.3B active. The stronger engineer of the two workers but it writes long, so on an 8 GB card with the experts in RAM a task takes about five minutes. Needs 24 GB of RAM beside a small card; fast on 16 GB cards.",
    [art("Qwen3-Coder-30B-A3B-Instruct-UD-Q4_K_XL.gguf", 17_665_334_432)], { nCtx: "8192", temperature: "0.2", minRamGb: "24" }),
];

const catalog: Catalog = {
  models: CATALOG,
  defaultChatModel: "qwen3-8b-q4km",
  defaultWorkerModel: "gpt-oss-20b",
  defaultSpeechModel: "whisper-small",
  defaultImageModel: "sd-turbo",
  defaultVideoModel: "wan2.1-t2v-1.3b",
};

// ------------------------------------------------------------------ installed models

const meta = (architecture: string, name: string, fileBytes: number, layers: number, contextLength: number, expertCount = 0) => ({
  architecture,
  name,
  fileBytes,
  layers,
  contextLength,
  expertCount,
  mixtureOfExperts: expertCount > 1,
  embeddingModel: architecture === "nomic-bert",
});

function installedFromCatalog(id: string, shared = false, daysAgo = 3): LocalModel {
  const m = CATALOG.find((c) => c.id === id)!;
  const bytes = m.artifacts.reduce((s, a) => s + a.bytes, 0);
  const file = `${shared ? SHARED : MODELS}\\${m.artifacts[0].file}`;
  const known: Record<string, ReturnType<typeof meta>> = {
    "gpt-oss-20b": meta("gpt-oss", "gpt-oss-20b", bytes, 24, 131_072, 32),
    "qwen3-coder-30b-a3b": meta("qwen3moe", "Qwen3-Coder-30B-A3B-Instruct", bytes, 48, 262_144, 128),
    "qwen3-8b-q4km": meta("qwen3", "Qwen3-8B", bytes, 36, 40_960),
    "qwen3-4b-q4km": meta("qwen3", "Qwen3-4B", bytes, 36, 40_960),
    "llama-3.1-8b-instruct-q4km": meta("llama", "Meta-Llama-3.1-8B-Instruct", bytes, 32, 131_072),
    "nomic-embed-text-v1.5-q8": meta("nomic-bert", "nomic-embed-text-v1.5", bytes, 12, 2048),
  };
  return {
    id,
    displayName: m.displayName,
    family: m.family,
    task: m.task,
    file,
    bytes,
    sha256: null,
    source: "catalog",
    downloadedAt: new Date(Date.now() - daysAgo * 86_400_000).toISOString(),
    metadata: known[id] ?? null,
    shared,
    unsupported: null,
  };
}

let installed: LocalModel[] = [];

// ------------------------------------------------------------------ the library's downloads

const state: DownloadsState = {
  isLoading: false,
  downloadingModels: [],
  pausedModels: [],
  stoppingModels: [],
  deletingModels: [],
  downloadingProgress: {},
  textDownloadingCount: 0,
};

function seed(): void {
  if (mockFlag("models") === "none") return;
  installed = [
    installedFromCatalog("gpt-oss-20b", false, 9),
    installedFromCatalog("qwen3-coder-30b-a3b", false, 6),
    installedFromCatalog("qwen3-8b-q4km", false, 12),
    installedFromCatalog("whisper-small", false, 12),
    installedFromCatalog("nomic-embed-text-v1.5-q8", false, 12),
    // Found in the installed Nook's models folder: usable here, deleted only from Nook itself.
    installedFromCatalog("llama-3.1-8b-instruct-q4km", true, 30),
    {
      id: "gemma-3-12b",
      displayName: "Gemma 3 12B",
      family: "gemma3",
      task: "chat",
      file: `${SHARED}\\gemma-3-12b-it-Q4_K_M.gguf`,
      bytes: 7_300_775_712,
      sha256: null,
      source: "local",
      downloadedAt: null,
      metadata: meta("gemma3", "gemma-3-12b-it", 7_300_775_712, 48, 131_072),
      shared: true,
      unsupported: null,
    },
    // A vision encoder downloaded from the Hub as if it were a model: listed so it can be deleted.
    {
      id: "deepseek-v4-deepseekv4flashvisionencodergguf",
      displayName: "deepseek-v4 DeepSeek-V4-Flash-Vision-Encoder.gguf",
      family: "deepseek-v4-gguf",
      task: UNSUPPORTED,
      file: `${MODELS}\\hub\\antirez-deepseek-v4-gguf\\DeepSeek-V4-Flash-Vision-Encoder.gguf`,
      bytes: 932_857_760,
      sha256: null,
      source: "huggingface",
      downloadedAt: new Date(Date.now() - 86_400_000).toISOString(),
      metadata: meta("deepseek4-vision", "DeepSeek V4 Flash Vision Encoder", 932_857_760, 32, 8192),
      shared: false,
      unsupported: "its architecture 'deepseek4-vision' is a vision encoder or projector, not a language model",
    },
  ];
  state.downloadingModels = ["qwen3-4b-q4km"];
  state.downloadingProgress = { "qwen3-4b-q4km": 0.18, "wan2.1-t2v-1.3b": 0.42 };
  state.pausedModels = ["wan2.1-t2v-1.3b"];
}

const snapshot = (): DownloadsState => ({
  ...state,
  downloadingModels: [...state.downloadingModels],
  pausedModels: [...state.pausedModels],
  stoppingModels: [...state.stoppingModels],
  deletingModels: [...state.deletingModels],
  downloadingProgress: { ...state.downloadingProgress },
  // One per catalog download job running, as the service counts them.
  textDownloadingCount: state.downloadingModels.length,
});

const changed = () => mockEmit("downloads", null);
const remove = (list: string[], key: string) => list.filter((k) => k !== key);
const bytesOf = (id: string) => CATALOG.find((c) => c.id === id)?.artifacts.reduce((s, a) => s + a.bytes, 0) ?? 1e9;

/** About 300 MB/s, so a download is watchable in the browser. */
const PER_TICK = 120_000_000;
let timer: number | undefined;

/** Hub downloads in flight: the variant and repo behind each runtime download key. */
const hubJobs = new Map<string, { repo: Repo; variant: Variant }>();
/** Downloaded hub variants ("<repo>:<variant key>") and the model id each became. */
const hubInstalled = new Map<string, string>();

function tick(): void {
  for (const id of [...state.downloadingModels]) {
    const p = Math.min(1, (state.downloadingProgress[id] ?? 0) + PER_TICK / bytesOf(id));
    state.downloadingProgress[id] = p;
    if (p >= 1) {
      state.downloadingModels = remove(state.downloadingModels, id);
      delete state.downloadingProgress[id];
      installed = [...installed, installedFromCatalog(id, false, 0)];
    }
  }
  for (const [key, job] of hubJobs) {
    const p = Math.min(1, (runtimeMock.downloads.get(key) ?? 0) + PER_TICK / job.variant.totalBytes);
    runtimeMock.downloads.set(key, p);
    if (p >= 1) {
      runtimeMock.downloads.delete(key);
      hubJobs.delete(key);
      hubInstalled.set(`${job.repo.id}:${job.variant.key}`, hubModelId(job.repo, job.variant));
      installed = [...installed, hubModel(job.repo, job.variant)];
      runtimeMock.event("model_downloaded", key, hubModelId(job.repo, job.variant));
    }
  }
  if (state.downloadingModels.length === 0 && hubJobs.size === 0) {
    window.clearInterval(timer);
    timer = undefined;
  }
  changed();
}

function run(): void {
  if (timer === undefined) timer = window.setInterval(tick, 400);
}

// ------------------------------------------------------------------ Hugging Face

const repo = (id: string, downloads: number, likes: number, pipelineTag: string | null = "text-generation", gated = false): Repo => {
  const [author, name] = id.split("/");
  return { id, author, name, downloads, likes, lastModified: "2026-08-14T09:12:00Z", gated, pipelineTag, tags: ["gguf"] };
};

const REPOS: Repo[] = [
  repo("bartowski/Meta-Llama-3.1-8B-Instruct-GGUF", 2_950_000, 890),
  repo("unsloth/Qwen3-8B-GGUF", 1_840_000, 312),
  repo("nomic-ai/nomic-embed-text-v1.5-GGUF", 1_780_000, 260, "sentence-similarity"),
  repo("MaziyarPanahi/Mistral-7B-Instruct-v0.3-GGUF", 1_350_000, 110),
  repo("unsloth/gemma-3-12b-it-GGUF", 1_120_000, 402, "image-text-to-text"),
  repo("unsloth/Qwen3-30B-A3B-GGUF", 980_000, 560),
  repo("unsloth/DeepSeek-R1-0528-Qwen3-8B-GGUF", 640_000, 310),
  repo("lmstudio-community/Qwen3-Coder-30B-A3B-Instruct-GGUF", 520_000, 140),
  repo("microsoft/phi-4-gguf", 410_000, 290),
  repo("google/gemma-3-4b-it-qat-q4_0-gguf", 402_000, 780, "image-text-to-text", true),
  repo("Qwen/Qwen3-Embedding-0.6B-GGUF", 260_000, 480, "feature-extraction"),
  repo("unsloth/GLM-4-9B-0414-GGUF", 88_000, 64),
  repo("someone/broken-upload-GGUF", 1_200, 2),
];

/** Billions of parameters per repository, for sizing its files. */
const PARAMS: Record<string, number> = {
  "bartowski/Meta-Llama-3.1-8B-Instruct-GGUF": 8,
  "unsloth/Qwen3-8B-GGUF": 8.2,
  "nomic-ai/nomic-embed-text-v1.5-GGUF": 0.137,
  "MaziyarPanahi/Mistral-7B-Instruct-v0.3-GGUF": 7.2,
  "unsloth/gemma-3-12b-it-GGUF": 12,
  "unsloth/Qwen3-30B-A3B-GGUF": 30.5,
  "unsloth/DeepSeek-R1-0528-Qwen3-8B-GGUF": 8.2,
  "lmstudio-community/Qwen3-Coder-30B-A3B-Instruct-GGUF": 30.5,
  "microsoft/phi-4-gguf": 14.7,
  "google/gemma-3-4b-it-qat-q4_0-gguf": 4,
  "Qwen/Qwen3-Embedding-0.6B-GGUF": 0.6,
  "unsloth/GLM-4-9B-0414-GGUF": 9.4,
};

const QUANTS: [string, number][] = [
  ["Q2_K", 2.96],
  ["Q3_K_M", 3.91],
  ["Q4_K_M", 4.85],
  ["Q5_K_M", 5.69],
  ["Q6_K", 6.56],
  ["Q8_0", 8.5],
  ["F16", 16],
];

function variant(base: string, label: string, bytes: number): Variant {
  const key = `${base}-${label}`;
  // Files over 30 GB come split, as the Hub's uploads are.
  const count = bytes > 30e9 ? Math.ceil(bytes / 25e9) : 1;
  const files = Array.from({ length: count }, (_, i) => ({
    path: count > 1 ? `${label}/${key}-0000${i + 1}-of-0000${count}.gguf` : `${key}.gguf`,
    bytes: Math.round(bytes / count),
    sha256: null,
    quant: label,
    shardIndex: count > 1 ? i + 1 : 0,
    shardCount: count > 1 ? count : 0,
    shardBase: key,
  }));
  return { label, key, files, totalBytes: files.reduce((s, f) => s + f.bytes, 0) };
}

function variantsOf(repoId: string): Variant[] {
  const params = PARAMS[repoId];
  if (params == null) return [];
  const base = repoId.split("/")[1].replace(/[-_]?gguf$/i, "");
  if (repoId.startsWith("google/gemma-3-4b-it-qat")) return [variant("gemma-3-4b-it-q4_0", "Q4_0", 3_155_051_328)];
  const quants = params < 1 ? QUANTS.filter(([l]) => ["Q4_K_M", "Q8_0", "F16"].includes(l)) : QUANTS;
  const out = quants.map(([label, bpw]) => variant(base, label, (params * 1e9 * bpw) / 8));
  // Two model files with one quantisation: their file names tell them apart.
  if (repoId === "unsloth/Qwen3-8B-GGUF") out.splice(3, 0, variant("Qwen3-8B-128K", "Q4_K_M", 5_027_784_736));
  return out;
}

/** HuggingFaceHub.modelId / displayName, enough for the mock's installed list. */
const hubModelId = (r: Repo, v: Variant) => `${r.name.replace(/[-_]?gguf$/i, "").toLowerCase()}-${v.label.toLowerCase().replace(/[^a-z0-9]/g, "")}`;

function hubModel(r: Repo, v: Variant): LocalModel {
  const name = r.name.replace(/[-_]?gguf$/i, "");
  const embed = /embed|bge/i.test(r.name) || r.pipelineTag === "feature-extraction" || r.pipelineTag === "sentence-similarity";
  return {
    id: hubModelId(r, v),
    displayName: `${name} ${v.label}`,
    family: r.name.toLowerCase(),
    task: embed ? "embed" : "chat",
    file: `${MODELS}\\hf\\${r.id.replace("/", "\\")}\\${v.files[0].path.split("/").pop()}`,
    bytes: v.totalBytes,
    sha256: null,
    source: "huggingface",
    downloadedAt: new Date().toISOString(),
    metadata: meta(embed ? "nomic-bert" : "llama", name, v.totalBytes, 32, 32_768),
    shared: false,
    unsupported: null,
  };
}

function search(query: string, limit: number): Repo[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const hits = REPOS.filter((r) => words.every((w) => r.id.toLowerCase().includes(w)));
  // An empty search is the popular models; the broken upload only turns up when asked for.
  return (words.length === 0 ? hits.filter((r) => PARAMS[r.id] != null) : hits).slice(0, limit).map((r) => ({ ...r }));
}

// ------------------------------------------------------------------ workers and web access

const prefs: Record<string, string> = {};
let webOn = true;

async function current(): Promise<Record<string, string>> {
  const out: Record<string, string> = {};
  const first = (task: string, preferred: string | null) => {
    const serving = installed.filter((m) => m.task === task);
    return serving.find((m) => m.id === prefs[task]) ?? serving.find((m) => m.id === preferred) ?? serving[0];
  };
  // Code's worker is whatever Code works with now (its own menu changes it too).
  const code = await call<CodeSnapshot>("code_snapshot").catch(() => null);
  const worker = code?.workerId ?? installed.find((m) => m.id === prefs.code)?.id;
  if (worker) out.code = worker;
  for (const [task, preferred] of [
    ["chat", catalog.defaultChatModel],
    ["embed", "nomic-embed-text-v1.5-q8"],
    ["speech", catalog.defaultSpeechModel],
    ["image", catalog.defaultImageModel],
    ["video", catalog.defaultVideoModel],
  ] as const) {
    const m = first(task, preferred);
    if (m) out[task] = m.id;
  }
  return out;
}

// ------------------------------------------------------------------ registration

export function registerModelsMocks(): void {
  seed();
  runtimeMock.installedIds = () => installed.map((m) => m.id);
  if (state.downloadingModels.length > 0) run();

  mock("models_catalog", async () => {
    await wait(250);
    return { ...catalog, models: catalog.models.map((m) => ({ ...m })) };
  });
  mock("models_installed", () => installed.map((m) => ({ ...m })));
  mock("models_downloads", () => snapshot());

  mock("models_download", ({ id }) => {
    const key = String(id);
    if (!CATALOG.some((c) => c.id === key)) throw new Error(`Unknown catalog model ${key}`);
    state.pausedModels = remove(state.pausedModels, key);
    if (!state.downloadingModels.includes(key)) state.downloadingModels = [...state.downloadingModels, key];
    state.downloadingProgress[key] = state.downloadingProgress[key] ?? 0;
    changed();
    run();
  });
  mock("models_pause", ({ id }) => {
    const key = String(id);
    state.downloadingModels = remove(state.downloadingModels, key);
    if (!state.pausedModels.includes(key)) state.pausedModels = [...state.pausedModels, key];
    changed();
  });
  mock("models_resume", ({ id }) => {
    const key = String(id);
    state.pausedModels = remove(state.pausedModels, key);
    if (!state.downloadingModels.includes(key)) state.downloadingModels = [...state.downloadingModels, key];
    changed();
    run();
  });
  mock("models_cancel", ({ id }) => {
    const key = String(id);
    state.stoppingModels = [...state.stoppingModels, key];
    state.downloadingModels = remove(state.downloadingModels, key);
    state.pausedModels = remove(state.pausedModels, key);
    delete state.downloadingProgress[key];
    changed();
    window.setTimeout(() => {
      state.stoppingModels = remove(state.stoppingModels, key);
      changed();
    }, 900);
  });
  mock("models_delete", ({ id }) => {
    const key = String(id);
    const m = installed.find((x) => x.id === key);
    if (m?.shared) {
      throw new Error(`${m.displayName} is in the previous Nook's models folder (${SHARED}), which this Nook only reads. Delete it from thok itself.`);
    }
    state.deletingModels = [...state.deletingModels, key];
    changed();
    window.setTimeout(() => {
      runtimeMock.unload(key);
      installed = installed.filter((x) => x.id !== key);
      for (const [variantId, modelId] of [...hubInstalled]) if (modelId === key) hubInstalled.delete(variantId);
      state.deletingModels = remove(state.deletingModels, key);
      changed();
    }, 700);
  });

  mock("hub_search", async ({ query, limit }) => {
    await wait(600);
    if (mockFlag("hub") === "fail") throw new Error("Could not reach huggingface.co: connect timed out");
    return search(String(query ?? ""), Number(limit ?? 30));
  });
  mock("hub_variants", async ({ repoId }) => {
    await wait(500);
    return variantsOf(String(repoId));
  });
  mock("hub_installed", ({ repoId, variants }) =>
    (variants as Variant[]).filter((v) => hubInstalled.has(`${String(repoId)}:${v.key}`)).map((v) => v.key),
  );
  mock("hub_download", ({ repo, variant }) => {
    const r = repo as Repo;
    const v = variant as Variant;
    const key = hubKey(r.id, v.key);
    if (runtimeMock.downloads.has(key)) return false;
    runtimeMock.downloads.set(key, 0);
    hubJobs.set(key, { repo: r, variant: v });
    changed();
    run();
    return true;
  });
  mock("hub_cancel", ({ repoId, variantKey }) => {
    const key = hubKey(String(repoId), String(variantKey));
    if (!hubJobs.delete(key)) return;
    runtimeMock.downloads.delete(key);
    runtimeMock.event("download_cancelled", key, null);
    changed();
  });

  mock("workers_preferences", () => ({ ...prefs }));
  mock("workers_current", () => current());
  mock("workers_set", async ({ task, modelId }) => {
    const t = String(task);
    if (modelId == null || modelId === "") delete prefs[t];
    else prefs[t] = String(modelId);
    // The code worker is Code's own setting too.
    if (t === "code") await call("code_set_worker", { modelId: modelId ?? catalog.defaultWorkerModel }).catch(() => undefined);
  });

  mock("web_access_enabled", () => webOn);
  mock("web_access_set", async ({ enabled }) => {
    await wait(150);
    webOn = enabled === true;
  });
}
