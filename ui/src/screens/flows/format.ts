/** The Flows page's wording (TranslateAudioFlow.kt helpers), kept pure for the tests. */
import type { Language, Need, Plan, Run } from "../../api/flows";
import { clock } from "../video/format";

export { clock, shortTime } from "../video/format";

/** "81 MB", "2.1 GB". */
export function bytesText(bytes: number): string {
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(1, Math.round(bytes / 1e6))} MB`;
}

/** A language's name from the table, or the code with a capital when Nook does not list it. */
export function languageName(code: string | null | undefined, languages: Language[]): string {
  if (!code) return "";
  const hit = languages.find((l) => l.code === code.toLowerCase());
  return hit ? hit.name : code.charAt(0).toUpperCase() + code.slice(1);
}

/**
 * The language to translate into at first: the computer's own, since what comes in is mostly in
 * another; Spanish when the computer is in English, since then what is said is likely English.
 */
export function defaultTarget(locale: string | undefined, languages: Language[]): string {
  const code = (locale ?? "en").split(/[-_]/)[0].toLowerCase();
  if (code === "en" || !languages.some((l) => l.code === code)) return "es";
  return code;
}

/** What a running translation is doing, above its progress bar. */
export function stageText(run: Run): string {
  switch (run.stage) {
    case "PREPARING":
      return run.source === "MICROPHONE" ? "Getting the recording ready" : "Preparing the audio";
    case "LISTENING":
      return run.total > 1 ? `Listening · part ${run.done + 1} of ${run.total}` : "Listening";
    case "TRANSLATING":
      return run.total > 0 ? `Translating · ${run.done} of ${run.total} lines` : "Translating";
    case "SPEAKING":
      return run.total > 0 ? `Speaking · ${run.done} of ${run.total} lines` : "Speaking";
    case "ASSEMBLING":
      return "Putting the track together";
    case "SAVING":
      return "Saving";
    default:
      return "Starting";
  }
}

/** A finished run's line: how much speech, how many lines, how long it took, and the model. */
export function doneText(run: Run): string {
  const lines = run.segments.length;
  return [
    `${clock(run.durationSeconds * 1000)} of speech`,
    `${lines} ${lines === 1 ? "line" : "lines"}`,
    `done in ${clock(run.elapsedMs)}`,
    run.modelId,
  ]
    .filter(Boolean)
    .join(" · ");
}

/** Who spoke the track: "Qwen3-TTS, in your own voice". */
export function voiceText(run: Run): string | null {
  if (!run.voiceName) return null;
  if (!run.cloned) return `${run.voiceName}, a standard voice`;
  return run.source === "MICROPHONE" ? `${run.voiceName}, in your own voice` : `${run.voiceName}, in the speaker's own voice`;
}

/** The one-time download line: every need with its size, and the whole when there are several. */
export function needsText(plan: Plan): string {
  const each = plan.needs.map((n: Need) => `${n.what} (${bytesText(n.bytes)})`).join(", ");
  const all = plan.needs.length > 1 ? `, ${bytesText(plan.totalBytes)} in all` : "";
  return `One-time download: ${each}${all}. It stays on this computer, and everything runs here.`;
}

/** The translation as plain text, one line per segment, for the clipboard. */
export function translationText(run: Run): string {
  return run.segments
    .map((s) => (s.translation ?? "").trim())
    .filter(Boolean)
    .join("\n");
}

/** A file's name and its folder from a Windows or POSIX path. */
export function splitPath(path: string): { name: string; folder: string } {
  const i = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  return i < 0 ? { name: path, folder: "" } : { name: path.slice(i + 1), folder: path.slice(0, i) };
}
