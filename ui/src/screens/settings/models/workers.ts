/** The Workers page's wording, kept pure for the tests (WorkerModelsView.kt). */
import { modelLabel } from "../../../components/activityFormat";
import { CODE_WORKER, type LocalModel } from "../../../api/models";
import { fastEnoughToWork, MIN_WORKER_TPS, type SpeedProbeResult } from "../../../api/runtime";
import { hubSize } from "./hub";
import { latestProbe } from "./library";

export interface WorkerTask {
  task: string;
  title: string;
  description: string;
}

export const WORKER_TASKS: readonly WorkerTask[] = [
  {
    task: CODE_WORKER,
    title: "Writing code",
    description:
      "Does Nook Code's work: each change is made in a scratch copy of the repository, verified and handed back as a diff. Only models made for tool use qualify; the plain chat model is not one.",
  },
  { task: "speech", title: "Transcribing", description: "Turns audio into text for the voice prompt." },
];

export const AUTOMATIC = "Automatic";

export const writesCode = (t: WorkerTask) => t.task === CODE_WORKER;

const nameOf = (m: LocalModel) => (m.displayName.trim() === "" ? modelLabel(m.id) : m.displayName);

/** "gpt-oss 20B · 12.1 GB": a model as the dropdown lists it. */
export function labelFor(m: LocalModel): string {
  return `${nameOf(m)} · ${hubSize(m.bytes)}`;
}

/**
 * The installed models a task can take: Code takes a catalog model with the worker capability,
 * whatever its task; speech takes the speech models.
 */
export function candidatesFor(t: WorkerTask, installed: readonly LocalModel[], workerIds: ReadonlySet<string>): LocalModel[] {
  return writesCode(t) ? installed.filter((m) => workerIds.has(m.id)) : installed.filter((m) => m.task === t.task);
}

/**
 * "gpt-oss 20B: 31 tok/s measured on this card; experts in system RAM on an 8 GB card, about 24 GB
 * of it." or that it is not measured yet: the measured speed and the memory a worker needs, beside
 * the static fit (the Codex report's P8).
 */
export function speedAndMemory(m: LocalModel, probes: readonly SpeedProbeResult[]): string {
  const probe = latestProbe(probes, m.id);
  const tooSlow = ` (under ${Math.trunc(MIN_WORKER_TPS)}: too slow for Nook Code)`;
  const speed =
    probe == null
      ? "speed not measured yet (it is, on the first load)"
      : `${probe.generateTps.toFixed(0)} tok/s measured on this card` + (fastEnoughToWork(probe) ? "" : tooSlow);
  const memory = m.metadata?.mixtureOfExperts === true ? "experts in system RAM on an 8 GB card, about 24 GB of it" : "fits the card as placed";
  return `${nameOf(m)}: ${speed}; ${memory}.`;
}

/** The row's description: what the task is, what serves it now, and for Code each worker's speed. */
export function taskDescription(
  t: WorkerTask,
  candidates: readonly LocalModel[],
  servingId: string | null | undefined,
  probes: readonly SpeedProbeResult[],
): string {
  let text = t.description;
  const serving = candidates.find((m) => m.id === servingId);
  if (candidates.length === 0) {
    text += writesCode(t)
      ? " No worker installed yet: download gpt-oss 20B or Qwen3-Coder 30B-A3B under Models (24 GB of RAM beside an 8 GB card)."
      : " Nothing installed for this yet.";
  } else if (serving) {
    text += ` Now: ${labelFor(serving)}.`;
  }
  if (writesCode(t)) for (const m of candidates) text += ` ${speedAndMemory(m, probes)}`;
  return text;
}

/** The dropdown's choice: the remembered model's label, else "Automatic". */
export function selectedLabel(candidates: readonly LocalModel[], chosenId: string | undefined): string {
  const chosen = candidates.find((m) => m.id === chosenId);
  return chosen ? labelFor(chosen) : AUTOMATIC;
}

/** The model id for a label picked in the dropdown; null for "Automatic". */
export function idForLabel(candidates: readonly LocalModel[], label: string): string | null {
  return candidates.find((m) => labelFor(m) === label)?.id ?? null;
}

/** WebAccessSetting.kt: what the switch means, on and off. */
export function webAccessDescription(on: boolean): string {
  return on
    ? "When a request needs something the repository cannot tell it (a library's API, an error message), the worker " +
        "searches DuckDuckGo and reads public pages over this computer's own internet connection, with no key or " +
        "account. It opens only addresses it was shown, never this computer or its network."
    : "The worker works with the repository alone, and Nook makes no web requests for it.";
}
