/**
 * The Browse page's arithmetic and wording, kept pure for the tests: HuggingFaceHub's static
 * helpers (`fit`, `size`, `Repo.looksLikeEmbedding`) as hugging_face_hub.rs has them, and the
 * words HubBrowserView.kt shows for them.
 */
import type { Fit, Repo, Variant } from "../../../api/models";
import { gpuMemory } from "../../../shell/platform";

/** Searches offered under the field, so a first look is one click. */
export const QUICK_SEARCHES = ["Qwen3", "Llama 3", "Gemma 3", "Mistral", "DeepSeek", "Phi-4", "GLM", "embedding"];

/** Working memory a loaded model needs beyond its weights: context, compute buffers, driver. */
const OVERHEAD_BYTES = 700 * 1024 * 1024;

const GIB = 1024 * 1024 * 1024;

/** HuggingFaceHub.fit: whether a model of this size runs comfortably on the largest GPU. */
export function fit(bytes: number, gpuTotalBytes: number): Fit {
  if (gpuTotalBytes <= 0) return "NO_GPU";
  const need = Math.trunc(bytes * 1.05) + OVERHEAD_BYTES;
  if (need <= gpuTotalBytes * 0.92) return "FITS";
  if (need <= gpuTotalBytes * 1.15) return "TIGHT";
  return "OFFLOAD";
}

/** A fit as its chip: the text and whether it is drawn in the accent. */
export function fitChip(f: Fit): [text: string, accent: boolean] {
  switch (f) {
    case "FITS":
      return ["Fits your GPU", true];
    case "TIGHT":
      return ["Tight fit", false];
    case "OFFLOAD":
      return ["Too big for the GPU", false];
    default:
      return ["CPU only", false];
  }
}

/** HuggingFaceHub.size: "36 MB" or "5.0 GB". */
export function hubSize(bytes: number): string {
  if (bytes < 1_000_000_000) return `${Math.round(bytes / 1e6)} MB`;
  return `${(bytes / 1e9).toFixed(1)} GB`;
}

/** Repo.looksLikeEmbedding. */
export function looksLikeEmbedding(repo: Repo): boolean {
  const name = repo.name.toLowerCase();
  return (
    repo.pipelineTag === "feature-extraction" ||
    repo.pipelineTag === "sentence-similarity" ||
    name.includes("embed") ||
    name.includes("bge")
  );
}

/** The repository's name without its "-GGUF" suffix. */
export function repoTitle(repo: Repo): string {
  return repo.name.replace(/[-_]?gguf$/i, "");
}

/** "unsloth · 1.2M downloads · 480 likes". */
export function repoFacts(repo: Repo, compact: (n: number) => string): string {
  return `${repo.author} · ${compact(repo.downloads)} downloads · ${compact(repo.likes)} likes`;
}

/** The caption under the quick searches: what the fit chips mean against this GPU. */
export function sizingNote(gpuTotalBytes: number): string {
  return (
    "Any GGUF on Hugging Face runs on Nook." +
    (gpuTotalBytes > 0
      ? ` Sized against ${(gpuTotalBytes / GIB).toFixed(0)} GB of ${gpuMemory}: Fits runs fully on the GPU, Tight keeps most layers on it, Too big runs mostly on the CPU.`
      : "")
  );
}

/** A variant's size, and its parts when split: "18.6 GB · 2 parts". */
export function variantSize(v: Variant): string {
  return hubSize(v.totalBytes) + (v.files.length > 1 ? ` · ${v.files.length} parts` : "");
}

/** Whether two variants of a repository share this one's label, so its file name must be shown. */
export function labelShared(variants: readonly Variant[], v: Variant): boolean {
  return variants.filter((o) => o.label === v.label).length > 1;
}

/** "42%": a download's percent, rounded as String.format("%.0f%%") does. */
export function percent(p: number): string {
  return `${Math.round(Math.max(0, Math.min(1, p)) * 100)}%`;
}
