/** HuggingFaceHub's helpers as the Browse page uses them (hugging_face_hub.rs tests, HubBrowserView.kt wording). */
import { describe, expect, it } from "vitest";
import type { Repo, Variant } from "../../../api/models";
import { hubKey } from "../../../api/models";
import { compact } from "../../../components/activityFormat";
import { fit, fitChip, hubSize, labelShared, looksLikeEmbedding, percent, repoFacts, repoTitle, sizingNote, variantSize } from "./hub";

const GIB = 1024 ** 3;
const repo = (name: string, pipelineTag: string | null = null): Repo => ({
  id: `someone/${name}`,
  author: "someone",
  name,
  downloads: 1_234_567,
  likes: 4_321,
  lastModified: null,
  gated: false,
  pipelineTag,
  tags: [],
});
const variant = (label: string, key: string, parts: number[]): Variant => ({
  label,
  key,
  files: parts.map((bytes, i) => ({ path: `${key}-${i}.gguf`, bytes, sha256: null, quant: label, shardIndex: i + 1, shardCount: parts.length, shardBase: key })),
  totalBytes: parts.reduce((a, b) => a + b, 0),
});

describe("the hub's helpers", () => {
  it("sizes a model against the largest GPU", () => {
    expect(fit(4_000_000_000, 0)).toBe("NO_GPU");
    expect(fit(5_000_000_000, 8 * GIB)).toBe("FITS");
    expect(fit(7_500_000_000, 8 * GIB)).toBe("TIGHT");
    expect(fit(12_000_000_000, 8 * GIB)).toBe("OFFLOAD");
    expect(fitChip("FITS")).toEqual(["Fits your GPU", true]);
    expect(fitChip("TIGHT")).toEqual(["Tight fit", false]);
    expect(fitChip("OFFLOAD")).toEqual(["Too big for the GPU", false]);
    expect(fitChip("NO_GPU")).toEqual(["CPU only", false]);
  });

  it("writes sizes in megabytes under a gigabyte", () => {
    expect(hubSize(36_400_000)).toBe("36 MB");
    expect(hubSize(4_999_999_999)).toBe("5.0 GB");
    expect(variantSize(variant("Q8_0", "big-Q8_0", [9e9, 9.6e9]))).toBe("18.6 GB · 2 parts");
    expect(variantSize(variant("Q4_K_M", "m-Q4_K_M", [5e9]))).toBe("5.0 GB");
  });

  it("recognises embedding repositories by tag or name", () => {
    expect(looksLikeEmbedding(repo("nomic-embed-text-v1.5-GGUF"))).toBe(true);
    expect(looksLikeEmbedding(repo("bge-m3-GGUF"))).toBe(true);
    expect(looksLikeEmbedding(repo("anything", "sentence-similarity"))).toBe(true);
    expect(looksLikeEmbedding(repo("Qwen3-8B-GGUF", "text-generation"))).toBe(false);
  });

  it("names a repository without its GGUF suffix and lists its reach", () => {
    expect(repoTitle(repo("Qwen3-8B-GGUF"))).toBe("Qwen3-8B");
    expect(repoTitle(repo("phi-4_gguf"))).toBe("phi-4");
    expect(repoTitle(repo("gguf-tools"))).toBe("gguf-tools");
    expect(repoFacts(repo("x"), compact)).toBe("someone · 1.2M downloads · 4,321 likes");
  });

  it("shows the file name only where two files share a quantisation label", () => {
    const vs = [variant("Q4_K_M", "Qwen3-8B-Q4_K_M", [5e9]), variant("Q4_K_M", "Qwen3-8B-128K-Q4_K_M", [5e9]), variant("Q8_0", "Qwen3-8B-Q8_0", [8e9])];
    expect(labelShared(vs, vs[0])).toBe(true);
    expect(labelShared(vs, vs[2])).toBe(false);
  });

  it("keys hub downloads as the runtime does, and words the sizing note", () => {
    expect(hubKey("unsloth/Qwen3-8B-GGUF", "Qwen3-8B-Q4_K_M")).toBe("hub:unsloth/Qwen3-8B-GGUF:Qwen3-8B-Q4_K_M");
    expect(sizingNote(0)).toBe("Any GGUF on Hugging Face runs on Nook.");
    expect(sizingNote(8188 * 1024 * 1024)).toContain(" Sized against 8 GB of VRAM: Fits runs fully on the GPU,");
    expect(percent(0.425)).toBe("43%");
    expect(percent(1.2)).toBe("100%");
  });
});
