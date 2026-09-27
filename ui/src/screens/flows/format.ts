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

/** What a running run is doing, above its progress bar. */
export function stageText(run: Run): string {
  switch (run.stage) {
    case "PREPARING":
      return run.source === "MICROPHONE" ? "Getting the recording ready" : "Preparing the audio";
    case "READING":
      return run.source === "TEXT" ? "Reading the text" : "Reading the document";
    case "LISTENING":
      return run.total > 1 ? `Listening · part ${run.done + 1} of ${run.total}` : "Listening";
    case "TRANSLATING":
      return run.total > 0 ? `Translating · ${run.done} of ${run.total} lines` : "Translating";
    case "SUMMARIZING":
      if (run.flow === "transcribe") return "Writing the notes";
      return run.total > 1 ? `Summarizing · ${run.done} of ${run.total} steps` : "Summarizing";
    case "SPEAKING":
      if (run.flow === "read-aloud") return run.total > 0 ? `Reading aloud · ${run.done} of ${run.total} lines` : "Reading aloud";
      return run.total > 0 ? `Speaking · ${run.done} of ${run.total} lines` : "Speaking";
    case "ASSEMBLING":
      return run.flow === "read-aloud" ? "Putting the reading together" : "Putting the track together";
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

/** "2,480 words". */
export function wordsText(words: number): string {
  return `${words.toLocaleString("en-US")} ${words === 1 ? "word" : "words"}`;
}

/** About how long `words` take to read aloud, at 150 words a minute: "about 17 minutes". */
export function listenText(words: number): string {
  const minutes = Math.max(1, Math.round(words / 150));
  if (minutes < 60) return `about ${minutes} ${minutes === 1 ? "minute" : "minutes"}`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return `about ${hours} h${rest ? ` ${rest} min` : ""}`;
}

/** A finished Nooklet run's line: how much was heard or read, how long it took, and the model. */
export function nookletDoneText(run: Run): string {
  const parts: (string | null)[] =
    run.flow === "transcribe"
      ? [`${clock(run.durationSeconds * 1000)} of speech`, run.words ? wordsText(run.words) : null]
      : run.flow === "read-aloud"
        ? [run.words ? wordsText(run.words) : null, `${clock(run.durationSeconds * 1000)} to listen`]
        : [run.words ? `${wordsText(run.words)} read` : null];
  parts.push(`done in ${clock(run.elapsedMs)}`);
  if (run.flow !== "read-aloud") parts.push(run.summary ? run.modelId : null);
  return parts.filter(Boolean).join(" · ");
}

/** The transcript as text, with a time before each paragraph, for the clipboard and the card. */
export function transcriptParagraphs(run: Run, pause = 2): { at: number; text: string }[] {
  const out: { at: number; text: string }[] = [];
  let last: number | null = null;
  for (const s of run.segments) {
    const text = s.text.trim();
    if (!text) continue;
    const now = out[out.length - 1];
    if (!now || (last != null && s.start - last >= pause) || now.text.length > 700) out.push({ at: s.start, text });
    else now.text += ` ${text}`;
    last = s.end;
  }
  return out;
}

/** A file's name and its folder from a Windows or POSIX path. */
export function splitPath(path: string): { name: string; folder: string } {
  const i = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  return i < 0 ? { name: path, folder: "" } : { name: path.slice(i + 1), folder: path.slice(0, i) };
}
