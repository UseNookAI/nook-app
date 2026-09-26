/** ModelDownloadsStripTest.kt */
import { describe, expect, it } from "vitest";
import { downloadLines } from "./downloadLines";

const model = (id: string, name: string) => ({ model: id, fullName: name });

describe("downloadLines", () => {
  it("library downloads show their name and hub files their file", () => {
    const lines = downloadLines(
      [model("qwen3-coder-30b", "Qwen3 Coder 30B")],
      ["qwen3-coder-30b"],
      [],
      { "qwen3-coder-30b": 0.42 },
      { "hub:unsloth/Qwen3-8B-GGUF:Qwen3-8B-Q4_K_M": 0.1 },
      () => null,
    );
    expect(lines).toEqual([
      { name: "Qwen3 Coder 30B", progress: 0.42, paused: false },
      { name: "Qwen3-8B-Q4_K_M", progress: 0.1, paused: false },
    ]);
  });

  it("a paused download keeps its percent and one model is listed once", () => {
    const lines = downloadLines(
      [model("gpt-oss-20b", "gpt-oss 20B")],
      [],
      ["gpt-oss-20b"],
      { "gpt-oss-20b": 0.7 },
      { "gpt-oss-20b": 0.7, "whisper-small": 0.3 },
      (id) => (id === "whisper-small" ? "Whisper Small" : null),
    );
    expect(lines).toEqual([
      { name: "gpt-oss 20B", progress: 0.7, paused: true },
      { name: "Whisper Small", progress: 0.3, paused: false },
    ]);
  });

  it("nothing in flight is no lines", () => {
    expect(downloadLines([], [], [], {}, {}, () => null)).toEqual([]);
  });

  it("an unknown key is shown as itself and a missing progress as zero", () => {
    expect(downloadLines([], ["mystery"], [], {}, {}, () => null)).toEqual([{ name: "mystery", progress: 0, paused: false }]);
  });
});
