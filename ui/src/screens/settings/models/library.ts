/**
 * The Library's logic, kept pure for the tests: the catalog and the installed models as the cards
 * show them (NookAiService.getAllAvailableModels / toDto / uiTask, AiModelDto), the kind filter,
 * which section a model lands in, its line of facts and its chips (AiModelsSettingsView.kt).
 */
import { UNSUPPORTED, type Catalog, type CatalogModel, type DownloadsState, type LocalModel } from "../../../api/models";
import { fastEnoughToWork, type SpeedProbeResult } from "../../../api/runtime";
import { gpuMemory } from "../../../shell/platform";

/**
 * common/dto/AiModelDto.java, as NookAiService built it for the Library. Every model is in Nook's
 * own registry now, so `modelRegistry` is always "NOOK" and `uniqueKey` is the id. `shared` (new)
 * marks a model found in the installed Nook's models folder: installed, but not deletable here.
 * `unsupported` (new) is why Nook cannot run a file (`LocalModel.unsupported`): listed so it can
 * be deleted, never used.
 */
export interface AiModelDto {
  model: string;
  fullName: string | null;
  description: string | null;
  tasks: string[];
  requiredVramInGB: number | null;
  modelRegistry: "NOOK";
  sizeInGB: number | null;
  type: string;
  contextTokens: number;
  shared: boolean;
  unsupported: string | null;
}

/** The kind line of a file Nook cannot run. */
export const NOT_RUNNABLE = "Not a model Nook can run";

/** ModelDownloadService's `AiModelDto.uniqueKey`. */
export const uniqueKey = (m: AiModelDto) => m.model;

/** The kind filter. "Code" is the models Code can work with: the catalog's workers, and any installed chat model. */
export const KINDS = ["All", "Text", "Code", "Speech", "Video"] as const;
export type Kind = (typeof KINDS)[number];

/** Kinds Nook Code has no use for: listed once installed, so they can be removed, never offered for download. */
const UNUSED_TASKS = new Set(["image-generation", "embedding"]);

/**
 * NookAiService.uiTask: the UI task tag for a catalog task (chat, embed, speech, image, video), and
 * for a file Nook cannot run (`UNSUPPORTED`), which has no kind.
 */
export function uiTask(task: string | null | undefined): string {
  switch (task ?? "chat") {
    case "embed":
      return "embedding";
    case "speech":
      return "speech-to-text";
    case "image":
      return "image-generation";
    case "video":
      return "video-generation";
    case UNSUPPORTED:
      return UNSUPPORTED;
    default:
      return "text-generation";
  }
}

const round1 = (v: number) => Math.round(v * 10) / 10;

/** NookAiService.toDto(CatalogModel). */
export function catalogDto(m: CatalogModel, shared = false): AiModelDto {
  const bytes = m.artifacts.reduce((sum, a) => sum + a.bytes, 0);
  return {
    model: m.id,
    fullName: m.displayName,
    description: m.description + (m.license.trim() === "" ? "" : ` Licence: ${m.license}.`),
    tasks: [uiTask(m.task), ...m.capabilities],
    requiredVramInGB: m.minVramGb,
    modelRegistry: "NOOK",
    sizeInGB: round1(bytes / 1e9),
    type: uiTask(m.task),
    contextTokens: Number.parseInt(m.defaults.nCtx ?? "", 10) || 8192,
    shared,
    unsupported: null,
  };
}

const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/**
 * NookAiService.toDto(LocalModel): a model placed in the models folder by hand (or from the Hub).
 * A file Nook cannot run says why and needs no memory.
 */
export function localDto(m: LocalModel): AiModelDto {
  const detail = m.metadata
    ? `${m.metadata.architecture}, ${m.metadata.layers} layers, context ${m.metadata.contextLength}`
    : `${m.task} model`;
  const unsupported = m.unsupported ?? null;
  return {
    model: m.id,
    fullName: m.displayName,
    description:
      unsupported != null ? `Nook can't run it: ${unsupported}. Added from ${fileName(m.file)}.` : `Added from ${fileName(m.file)} (${detail}).`,
    tasks: [uiTask(m.task), "local-file"],
    requiredVramInGB: unsupported != null ? null : Math.ceil(m.bytes / 1e9) + 1,
    modelRegistry: "NOOK",
    sizeInGB: Math.round(m.bytes / 1e8) / 10,
    type: uiTask(m.task),
    contextTokens: m.metadata && unsupported == null ? m.metadata.contextLength : 0,
    shared: m.shared,
    unsupported,
  };
}

/**
 * NookAiService.getAllAvailableModels: everything in the catalog, then any model placed in the
 * models folder by hand (still shown and manageable).
 */
export function availableModels(catalog: Catalog | null, installed: readonly LocalModel[]): AiModelDto[] {
  const shared = new Set(installed.filter((m) => m.shared).map((m) => m.id));
  const all: AiModelDto[] = [];
  const seen = new Set<string>();
  for (const m of catalog?.models ?? []) {
    all.push(catalogDto(m, shared.has(m.id)));
    seen.add(m.id);
  }
  for (const local of installed) {
    if (!seen.has(local.id)) {
      seen.add(local.id);
      all.push(localDto(local));
    }
  }
  return all;
}

/** "650 MB" under a gigabyte, else "4.9 GB". */
export function sizeText(gb: number | null): string | null {
  if (gb == null) return null;
  if (gb < 1.0) return `${Math.trunc(gb * 1024)} MB`;
  return `${gb.toFixed(1)} GB`;
}

export function kindText(model: AiModelDto): string | null {
  const tasks = model.tasks;
  if (tasks.includes(UNSUPPORTED)) return NOT_RUNNABLE;
  if (tasks.includes("text-generation")) return "Text";
  if (tasks.includes("image-generation")) return "Images";
  if (tasks.includes("speech-to-text")) return "Speech";
  if (tasks.includes("video-generation")) return "Video";
  if (tasks.includes("embedding")) return "Embeddings";
  return null;
}

/** The card's line of facts: "4.9 GB · Text · needs 6 GB VRAM". */
export function metaFor(model: AiModelDto): string {
  const vram = model.requiredVramInGB;
  return [sizeText(model.sizeInGB), kindText(model), vram != null && vram > 0 ? `needs ${vram} GB ${gpuMemory}` : null]
    .filter((s): s is string => s != null)
    .join(" · ");
}

/** Whether a model is installed, by name, with the ":latest" spellings the service allowed. */
export function isInstalled(model: AiModelDto, installedNames: ReadonlySet<string>): boolean {
  const id = model.model;
  if (installedNames.has(id)) return true;
  if (!id.includes(":") && installedNames.has(`${id}:latest`)) return true;
  for (const name of installedNames) if (name.startsWith(`${id}:`)) return true;
  return id.endsWith(":latest") && installedNames.has(id.slice(0, -":latest".length));
}

/** Whether a model belongs under the kind filter. `codeChoices` are the models Code's own menu offers. */
export function matchesKind(model: AiModelDto, kind: Kind, codeChoices: ReadonlySet<string>): boolean {
  const tasks = model.tasks;
  switch (kind) {
    case "Text":
      return tasks.includes("text-generation");
    // The tested workers, to download or use, and any installed model Code can use.
    case "Code":
      return tasks.includes("worker") || codeChoices.has(model.model);
    case "Speech":
      return tasks.includes("speech-to-text");
    case "Video":
      return tasks.includes("video-generation");
    default:
      return true;
  }
}

export function matchesSearch(model: AiModelDto, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (q === "") return true;
  return (
    model.model.toLowerCase().includes(q) ||
    (model.fullName ?? "").toLowerCase().includes(q) ||
    (model.description ?? "").toLowerCase().includes(q)
  );
}

export interface LibrarySections {
  installing: AiModelDto[];
  installed: AiModelDto[];
  available: AiModelDto[];
}

/**
 * Filters the library by the search and the kind, then splits it: downloads in flight (running,
 * paused or stopping), installed models, and what can be downloaded (never an image or embedding
 * model: those are only listed once installed, so they can be removed).
 */
export function librarySections(
  models: readonly AiModelDto[],
  installedNames: ReadonlySet<string>,
  downloads: DownloadsState,
  query: string,
  kind: Kind,
  codeChoices: ReadonlySet<string>,
): LibrarySections {
  const inFlight = new Set([...downloads.downloadingModels, ...downloads.pausedModels, ...downloads.stoppingModels]);
  const seen = new Set<string>();
  const out: LibrarySections = { installing: [], installed: [], available: [] };
  for (const model of models) {
    if (!matchesSearch(model, query) || !matchesKind(model, kind, codeChoices)) continue;
    const key = uniqueKey(model);
    if (seen.has(key)) continue;
    seen.add(key);
    if (isInstalled(model, installedNames)) out.installed.push(model);
    else if (inFlight.has(key)) out.installing.push(model);
    else if (!model.tasks.some((t) => UNUSED_TASKS.has(t))) out.available.push(model);
  }
  return out;
}

/** What the chips need to know beyond the model. */
export interface ChipContext {
  /** The model Code works with now. */
  codeWorker: string | null;
  /** Models with a READY engine. */
  residentIds: ReadonlySet<string>;
  defaultChatModel: string | null;
  /** The first GPU's memory in GiB, when read. */
  gpuTotalGb: number | null;
  /** The speed probe's results (RuntimeManager.Status.probes). */
  probes: readonly SpeedProbeResult[];
}

/** A chip: its text and whether it is drawn in the accent. */
export type ChipSpec = [text: string, accent: boolean];

/**
 * A card's chips, the one that matters most first. Beyond the Kotlin card: the measured speed of an
 * installed text model ("31 tok/s", or "Too slow for Code" under SpeedProbe.MIN_WORKER_TPS), and
 * "From Nook" on a model shared from the installed Nook's folder (why it has no Delete).
 */
export function chipsFor(model: AiModelDto, installed: boolean, ctx: ChipContext): ChipSpec[] {
  const chips: ChipSpec[] = [];
  if (installed && model.model === ctx.codeWorker) chips.push(["Used by Code", true]);
  if (installed && ctx.residentIds.has(model.model)) chips.push(["Loaded", true]);
  if (installed) {
    const probe = latestProbe(ctx.probes, model.model);
    if (probe) chips.push(fastEnoughToWork(probe) ? [`${Math.round(probe.generateTps)} tok/s`, false] : ["Too slow for Code", false]);
  }
  if (model.model === ctx.defaultChatModel) chips.push(["Default", false]);
  const vram = model.requiredVramInGB;
  if (!installed && ctx.gpuTotalGb != null && vram != null && vram <= ctx.gpuTotalGb) chips.push(["Fits your GPU", true]);
  if (installed && model.shared) chips.push(["From Nook", false]);
  return chips;
}

/** The newest probe of a model, or null. */
export function latestProbe(probes: readonly SpeedProbeResult[], modelId: string): SpeedProbeResult | null {
  let best: SpeedProbeResult | null = null;
  for (const p of probes) {
    if (p.modelId !== modelId) continue;
    if (best == null || Date.parse(p.measuredAt) > Date.parse(best.measuredAt)) best = p;
  }
  return best;
}
