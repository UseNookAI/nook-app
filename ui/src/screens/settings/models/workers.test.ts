/** WorkerModelsView.kt and WebAccessSetting.kt wording. */
import { describe, expect, it } from "vitest";
import type { LocalModel } from "../../../api/models";
import type { SpeedProbeResult } from "../../../api/runtime";
import { AUTOMATIC, candidatesFor, idForLabel, labelFor, selectedLabel, speedAndMemory, taskDescription, webAccessDescription, WORKER_TASKS } from "./workers";

const model = (id: string, displayName: string, task: string, bytes: number, moe = false): LocalModel => ({
  id,
  displayName,
  family: id,
  task,
  file: `C:\\models\\${id}.gguf`,
  bytes,
  sha256: null,
  source: "catalog",
  downloadedAt: null,
  metadata: task === "chat" ? { architecture: "x", name: id, fileBytes: bytes, layers: 24, contextLength: 8192, expertCount: moe ? 32 : 0, mixtureOfExperts: moe, embeddingModel: false } : null,
  shared: false,
  unsupported: null,
});
const probe = (modelId: string, generateTps: number): SpeedProbeResult => ({
  modelId,
  sha256: null,
  driver: "581.29",
  backend: "cuda",
  gpuLayers: -1,
  promptTps: 400,
  generateTps,
  measuredAt: "2026-09-24T10:00:00Z",
});

const gptOss = model("gpt-oss-20b", "gpt-oss 20B", "chat", 12_109_566_624, true);
const qwenCoder = model("qwen3-coder-30b-a3b", "Qwen3-Coder 30B-A3B", "chat", 17_665_334_432, true);
const qwen8 = model("qwen3-8b-q4km", "", "chat", 5_027_784_096);
const whisper = model("whisper-small", "Whisper Small", "speech", 487_601_967);
const installed = [gptOss, qwenCoder, qwen8, whisper];
const [code, speech] = WORKER_TASKS;

describe("worker models", () => {
  it("Code takes the catalog's workers and speech the speech models", () => {
    const workerIds = new Set(["gpt-oss-20b", "qwen3-coder-30b-a3b"]);
    expect(candidatesFor(code, installed, workerIds)).toEqual([gptOss, qwenCoder]);
    expect(candidatesFor(speech, installed, workerIds)).toEqual([whisper]);
  });

  it("labels a model with its name, or one made from its id, and its size", () => {
    expect(labelFor(gptOss)).toBe("gpt-oss 20B · 12.1 GB");
    expect(labelFor(qwen8)).toBe("Qwen3 8B · 5.0 GB");
    expect(labelFor(whisper)).toBe("Whisper Small · 488 MB");
  });

  it("says each worker's measured speed and where its experts live", () => {
    expect(speedAndMemory(gptOss, [probe("gpt-oss-20b", 31.4)])).toBe(
      "gpt-oss 20B: 31 tok/s measured on this card; experts in system RAM on an 8 GB card, about 24 GB of it.",
    );
    expect(speedAndMemory(qwenCoder, [probe("qwen3-coder-30b-a3b", 6.3)])).toBe(
      "Qwen3-Coder 30B-A3B: 6 tok/s measured on this card (under 8: too slow for Nook Code); experts in system RAM on an 8 GB card, about 24 GB of it.",
    );
    expect(speedAndMemory(qwen8, [])).toBe("Qwen3 8B: speed not measured yet (it is, on the first load); fits the card as placed.");
  });

  it("describes what serves a task now, or that nothing is installed for it", () => {
    expect(taskDescription(speech, [whisper], "whisper-small", [])).toBe("Turns audio into text for the voice prompt. Now: Whisper Small · 488 MB.");
    expect(taskDescription(speech, [], null, [])).toBe("Turns audio into text for the voice prompt. Nothing installed for this yet.");
    expect(taskDescription(code, [], null, [])).toContain(
      "No worker installed yet: download gpt-oss 20B or Qwen3-Coder 30B-A3B under Models (24 GB of RAM beside an 8 GB card).",
    );
    const text = taskDescription(code, [gptOss], "gpt-oss-20b", [probe("gpt-oss-20b", 31.4)]);
    expect(text.endsWith(" Now: gpt-oss 20B · 12.1 GB. gpt-oss 20B: 31 tok/s measured on this card; experts in system RAM on an 8 GB card, about 24 GB of it.")).toBe(true);
  });

  it("maps the dropdown's labels to ids, Automatic to none", () => {
    expect(selectedLabel([gptOss, qwenCoder], "qwen3-coder-30b-a3b")).toBe("Qwen3-Coder 30B-A3B · 17.7 GB");
    expect(selectedLabel([gptOss], "gone")).toBe(AUTOMATIC);
    expect(idForLabel([gptOss, qwenCoder], "gpt-oss 20B · 12.1 GB")).toBe("gpt-oss-20b");
    expect(idForLabel([gptOss], AUTOMATIC)).toBeNull();
  });

  it("explains the web access switch both ways", () => {
    expect(webAccessDescription(true)).toContain("searches DuckDuckGo and reads public pages");
    expect(webAccessDescription(false)).toBe("The worker works with the repository alone, and Nook makes no web requests for it.");
  });
});
