/** The Library's logic (AiModelsSettingsView.kt helpers, NookAiService.toDto / getAllAvailableModels). */
import { describe, expect, it } from "vitest";
import { EMPTY_DOWNLOADS, type Catalog, type CatalogModel, type LocalModel } from "../../../api/models";
import type { SpeedProbeResult } from "../../../api/runtime";
import {
  availableModels,
  catalogDto,
  chipsFor,
  isInstalled,
  librarySections,
  localDto,
  metaFor,
  NOT_RUNNABLE,
  sizeText,
  uiTask,
  type AiModelDto,
  type ChipContext,
} from "./library";

const catalogModel = (id: string, task: string, capabilities: string[], bytes: number, minVramGb: number, license = "Apache-2.0"): CatalogModel => ({
  id,
  displayName: id.toUpperCase(),
  family: id.split("-")[0],
  task,
  paramsB: 1,
  description: `${id} model.`,
  capabilities,
  artifacts: [{ file: `${id}.gguf`, url: `https://example/${id}.gguf`, sha256: null, bytes, format: "gguf" }],
  defaults: task === "chat" ? { nCtx: "8192" } : {},
  backends: {},
  license,
  minVramGb,
});

const local = (id: string, task: string, bytes: number, shared = false): LocalModel => ({
  id,
  displayName: id,
  family: id,
  task,
  file: `C:\\models\\${id}.gguf`,
  bytes,
  sha256: null,
  source: "local",
  downloadedAt: null,
  metadata: task === "chat" ? { architecture: "gemma3", name: id, fileBytes: bytes, layers: 48, contextLength: 131072, expertCount: 0, mixtureOfExperts: false, embeddingModel: false } : null,
  shared,
  unsupported: null,
});

/** The vision encoder a person downloaded from the Hub on 2026-09-26, as the registry lists it. */
const encoder: LocalModel = {
  ...local("encoder", "unsupported", 932_857_760),
  displayName: "deepseek-v4 DeepSeek-V4-Flash-Vision-Encoder.gguf",
  file: "C:\\models\\hub\\antirez-deepseek-v4-gguf\\DeepSeek-V4-Flash-Vision-Encoder.gguf",
  source: "huggingface",
  metadata: { architecture: "deepseek4-vision", name: "DeepSeek V4 Flash Vision Encoder", fileBytes: 932_857_760, layers: 32, contextLength: 8192, expertCount: 0, mixtureOfExperts: false, embeddingModel: false },
  unsupported: "its architecture 'deepseek4-vision' is a vision encoder or projector, not a language model",
};

const catalog: Catalog = {
  models: [
    catalogModel("qwen3-8b", "chat", ["tools"], 5_027_784_096, 6),
    catalogModel("gpt-oss-20b", "chat", ["tools", "worker"], 12_109_566_624, 8),
    catalogModel("whisper-small", "speech", ["transcription"], 487_601_967, 1, "MIT"),
    catalogModel("sd-turbo", "image", ["text-to-image"], 5_214_561_328, 4),
    catalogModel("nomic-embed", "embed", ["embeddings"], 146_000_000, 1),
    catalogModel("wan", "video", ["text-to-video"], 5_935_462_998, 8),
  ],
  defaultChatModel: "qwen3-8b",
  defaultWorkerModel: "gpt-oss-20b",
  defaultSpeechModel: "whisper-small",
  defaultImageModel: "sd-turbo",
  defaultVideoModel: "wan",
};

const byId = (models: AiModelDto[], id: string) => models.find((m) => m.model === id)!;

describe("the library's models", () => {
  it("maps catalog tasks to the UI's task tags", () => {
    expect(uiTask("embed")).toBe("embedding");
    expect(uiTask("speech")).toBe("speech-to-text");
    expect(uiTask("image")).toBe("image-generation");
    expect(uiTask("video")).toBe("video-generation");
    expect(uiTask("chat")).toBe("text-generation");
    expect(uiTask(null)).toBe("text-generation");
  });

  it("a catalog model carries its licence, its capabilities and its size to a tenth of a gigabyte", () => {
    const dto = catalogDto(catalog.models[1]);
    expect(dto.description).toBe("gpt-oss-20b model. Licence: Apache-2.0.");
    expect(dto.tasks).toEqual(["text-generation", "tools", "worker"]);
    expect(dto.sizeInGB).toBe(12.1);
    expect(dto.requiredVramInGB).toBe(8);
    expect(dto.contextTokens).toBe(8192);
    expect(catalogDto({ ...catalog.models[0], license: " " }).description).toBe("qwen3-8b model.");
  });

  it("a model placed by hand says where it came from", () => {
    const dto = localDto(local("gemma-3-12b", "chat", 7_300_000_000, true));
    expect(dto.description).toBe("Added from gemma-3-12b.gguf (gemma3, 48 layers, context 131072).");
    expect(dto.tasks).toEqual(["text-generation", "local-file"]);
    expect(dto.requiredVramInGB).toBe(9);
    expect(dto.sizeInGB).toBe(7.3);
    expect(dto.shared).toBe(true);
    expect(localDto(local("voice", "speech", 1)).description).toBe("Added from voice.gguf (speech model).");
  });

  it("a file Nook cannot run says so and what it is, with no kind and no memory to find", () => {
    expect(uiTask("unsupported")).toBe("unsupported");
    const dto = localDto(encoder);
    expect(dto.description).toBe(
      "Nook can't run it: its architecture 'deepseek4-vision' is a vision encoder or projector, not a language model. Added from DeepSeek-V4-Flash-Vision-Encoder.gguf.",
    );
    expect(dto.tasks).toEqual(["unsupported", "local-file"]);
    expect(dto.unsupported).toBe(encoder.unsupported);
    expect(dto.requiredVramInGB).toBeNull();
    expect(dto.contextTokens).toBe(0);
    expect(metaFor(dto)).toBe(`921 MB · ${NOT_RUNNABLE}`);
    expect(localDto(local("gemma-3-12b", "chat", 7e9)).unsupported).toBeNull();
    expect(catalogDto(catalog.models[0]).unsupported).toBeNull();
  });

  it("lists the catalog, then models placed by hand, each once, with the shared ones marked", () => {
    const all = availableModels(catalog, [local("qwen3-8b", "chat", 5e9, true), local("gemma-3-12b", "chat", 7e9)]);
    expect(all.map((m) => m.model)).toEqual(["qwen3-8b", "gpt-oss-20b", "whisper-small", "sd-turbo", "nomic-embed", "wan", "gemma-3-12b"]);
    expect(byId(all, "qwen3-8b").shared).toBe(true);
    expect(byId(all, "gpt-oss-20b").shared).toBe(false);
  });

  it("sizes under a gigabyte in megabytes and says what the model needs", () => {
    expect(sizeText(0.5)).toBe("512 MB");
    expect(sizeText(4.95)).toBe("5.0 GB");
    expect(sizeText(null)).toBeNull();
    expect(metaFor(catalogDto(catalog.models[0]))).toBe("5.0 GB · Text · needs 6 GB VRAM");
    // 487.6 MB is kept as 0.5 GB, so it reads 512 MB, as the Kotlin card had it.
    expect(metaFor(catalogDto(catalog.models[2]))).toBe("512 MB · Speech · needs 1 GB VRAM");
    expect(metaFor({ ...catalogDto(catalog.models[2]), requiredVramInGB: 0 })).toBe("512 MB · Speech");
  });

  it("matches installed names with the :latest spellings", () => {
    const dto = catalogDto(catalog.models[0]);
    expect(isInstalled(dto, new Set(["qwen3-8b"]))).toBe(true);
    expect(isInstalled(dto, new Set(["qwen3-8b:latest"]))).toBe(true);
    expect(isInstalled(dto, new Set(["qwen3-8b:q4"]))).toBe(true);
    expect(isInstalled({ ...dto, model: "qwen3-8b:latest" }, new Set(["qwen3-8b"]))).toBe(true);
    expect(isInstalled(dto, new Set(["qwen3-8b-instruct"]))).toBe(false);
  });
});

describe("the library's sections", () => {
  const models = availableModels(catalog, [local("gemma-3-12b", "chat", 7e9, true)]);
  const installed = new Set(["qwen3-8b", "gemma-3-12b", "nomic-embed"]);

  it("splits into installing, installed and available, never offering image or embedding models", () => {
    const s = librarySections(models, installed, { ...EMPTY_DOWNLOADS, pausedModels: ["wan"] }, "", "All", new Set());
    expect(s.installing.map((m) => m.model)).toEqual(["wan"]);
    expect(s.installed.map((m) => m.model)).toEqual(["qwen3-8b", "nomic-embed", "gemma-3-12b"]);
    expect(s.available.map((m) => m.model)).toEqual(["gpt-oss-20b", "whisper-small"]);
  });

  it("an installed model wins over a download of the same name", () => {
    const s = librarySections(models, installed, { ...EMPTY_DOWNLOADS, downloadingModels: ["qwen3-8b"] }, "", "All", new Set());
    expect(s.installing).toEqual([]);
    expect(s.installed.map((m) => m.model)).toContain("qwen3-8b");
  });

  it("the Code kind is the catalog's workers and whatever Code's menu offers", () => {
    const s = librarySections(models, installed, EMPTY_DOWNLOADS, "", "Code", new Set(["gemma-3-12b"]));
    expect([...s.installed, ...s.available].map((m) => m.model)).toEqual(["gemma-3-12b", "gpt-oss-20b"]);
  });

  it("a file Nook cannot run is listed among the installed, under no kind but All", () => {
    const withEncoder = availableModels(catalog, [local("gemma-3-12b", "chat", 7e9, true), encoder]);
    const names = new Set([...installed, "encoder"]);
    expect(librarySections(withEncoder, names, EMPTY_DOWNLOADS, "", "All", new Set()).installed.map((m) => m.model)).toContain("encoder");
    for (const kind of ["Text", "Code", "Speech", "Video"] as const) {
      const s = librarySections(withEncoder, names, EMPTY_DOWNLOADS, "", kind, new Set(["gemma-3-12b"]));
      expect(s.installed.map((m) => m.model)).not.toContain("encoder");
    }
  });

  it("filters by id, name and description, ignoring case", () => {
    expect(librarySections(models, installed, EMPTY_DOWNLOADS, "WHISPER", "All", new Set()).available.map((m) => m.model)).toEqual(["whisper-small"]);
    expect(librarySections(models, installed, EMPTY_DOWNLOADS, "added from", "All", new Set()).installed.map((m) => m.model)).toEqual(["gemma-3-12b"]);
    const speech = librarySections(models, installed, EMPTY_DOWNLOADS, "", "Speech", new Set());
    expect([...speech.installed, ...speech.available].map((m) => m.model)).toEqual(["whisper-small"]);
  });
});

describe("a card's chips", () => {
  const probe = (modelId: string, generateTps: number, measuredAt: string): SpeedProbeResult => ({
    modelId,
    sha256: null,
    driver: "581.29",
    backend: "cuda",
    gpuLayers: -1,
    promptTps: 400,
    generateTps,
    measuredAt,
  });
  const ctx: ChipContext = {
    codeWorker: "gpt-oss-20b",
    residentIds: new Set(["gpt-oss-20b"]),
    defaultChatModel: "qwen3-8b",
    // An RTX 4060: 8188 MiB.
    gpuTotalGb: 8188 / 1024,
    probes: [probe("gpt-oss-20b", 12.4, "2026-09-20T10:00:00Z"), probe("gpt-oss-20b", 31.4, "2026-09-24T10:00:00Z"), probe("qwen3-8b", 6.3, "2026-09-24T10:00:00Z")],
  };
  const dto = (id: string) => byId(availableModels(catalog, []), id);

  it("puts what Code uses and what is loaded first, then the speed and the default", () => {
    expect(chipsFor(dto("gpt-oss-20b"), true, ctx)).toEqual([
      ["Used by Code", true],
      ["Loaded", true],
      ["31 tok/s", false],
    ]);
    expect(chipsFor(dto("qwen3-8b"), true, ctx)).toEqual([
      ["Too slow for Code", false],
      ["Default", false],
    ]);
  });

  it("offers Fits your GPU only for a model to download whose VRAM need is within the card", () => {
    expect(chipsFor(dto("qwen3-8b"), false, ctx)).toEqual([
      ["Default", false],
      ["Fits your GPU", true],
    ]);
    // 8 GB needed against an 8188 MiB card: not quite, as the Kotlin page had it.
    expect(chipsFor(dto("gpt-oss-20b"), false, ctx)).toEqual([]);
    expect(chipsFor(dto("whisper-small"), false, { ...ctx, gpuTotalGb: null })).toEqual([]);
  });

  it("marks a model shared from the installed Nook", () => {
    expect(chipsFor({ ...dto("whisper-small"), shared: true }, true, ctx)).toEqual([["From Nook", false]]);
  });
});
