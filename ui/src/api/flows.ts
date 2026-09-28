/**
 * The Flows page (nook_core::flow, src-tauri/src/commands/flows.rs). Types mirror the Rust of
 * `FlowService` (Run, Status, Stage, Source, Plan, Need, Install) and `flow::subtitles::Segment`
 * field for field; instants are epoch milliseconds, paths absolute strings.
 *
 * Events on the topic "flows": `{ run }` when a run is added or moves on, `{ removed: id }` when one
 * is deleted, `{ install }` (an Install or null) while the downloads run. The topic "downloads"
 * (the runtime's) and "runtime" make the page read its plan again. Microphone levels arrive on
 * "speech", as for the voice prompt (`onSpeechLevel` in ./code).
 *
 * Three more Nooklets run on the same queue (`service_nooklets.rs`), each asked for with an
 * `Order`: Transcribe (a recording, written down, with notes when asked), Summarize (a document or
 * pasted text) and Read aloud (a document or pasted text, spoken as one track). Their runs are
 * `Run`s with `flow` set to "transcribe", "summarize" or "read-aloud".
 */
import { convertFileSrc } from "@tauri-apps/api/core";
import { call, inTauri, on } from "./ipc";

/** FlowService.Status. */
export type Status = "QUEUED" | "RUNNING" | "DONE" | "FAILED" | "CANCELLED";

/** FlowService.Stage: what a RUNNING run is doing. */
export type Stage =
  | "PREPARING"
  | "READING"
  | "LISTENING"
  | "TRANSLATING"
  | "SUMMARIZING"
  | "SPEAKING"
  | "ASSEMBLING"
  | "SAVING";

/** Where a run's input came from: a file, the microphone, or pasted text (both kept in the run's folder). */
export type Source = "FILE" | "MICROPHONE" | "TEXT";

/** The flows on the queue. */
export const TRANSLATE_AUDIO = "translate-audio";
export const TRANSCRIBE = "transcribe";
export const SUMMARIZE = "summarize";
export const READ_ALOUD = "read-aloud";

/** How long a summary or notes are. */
export type Length = "short" | "detailed";

/**
 * What the person asks of a Nooklet (FlowService Order).
 * - `language`: Transcribe, the spoken language (null: detect); Summarize, the language to write in
 *   (null: the document's); Read aloud, the text's language
 * - `notes`: Transcribe, also write notes; `length`, `focus`: the summary's or notes'
 * - `female`: Read aloud, a woman's voice (else a man's) where the voice has both
 */
export interface Order {
  flow: string;
  language: string | null;
  notes: boolean;
  length: Length;
  focus: string | null;
  female: boolean;
}

/** A document's or pasted text's language (when it can be told) and its words, before a run. */
export interface Peek {
  language: string | null;
  words: number;
}

/** One stretch of speech: where it is in the track (seconds), what was said, and its translation once made. */
export interface Segment {
  start: number;
  end: number;
  text: string;
  translation: string | null;
  /** Where the translation is heard in the dubbed track, once it is made (the track's own timeline). */
  spokenStart?: number;
  spokenEnd?: number;
}

/** FlowService.Run: one run, queued, working or done. The service replaces it as it moves on. */
export interface Run {
  id: string;
  flow: string;
  /** The file the run works on (for a recording, `<run folder>\recording.wav`). */
  input: string;
  /** What the card calls it: the file's name, or "Recording". */
  inputName: string;
  source: Source;
  /** The spoken language's code, or null to detect it. */
  sourceLanguage: string | null;
  targetLanguage: string;
  /** The chat model that translated, once known. */
  modelId: string | null;
  keepVoice: boolean;
  /** The voice that spoke, once known, and whether in the speaker's own voice. */
  voiceName: string | null;
  cloned: boolean;
  /** What the person should know: why no cloning, or why no sound. */
  note: string | null;
  status: Status;
  stage: Stage | null;
  /** Units finished in the stage (parts listened to, lines translated or spoken), with `total`. */
  done: number;
  total: number;
  /** What Whisper heard, as a code (or its name when Nook does not list it). */
  detectedLanguage: string | null;
  durationSeconds: number;
  segments: Segment[];
  /** The translated track (16-bit WAV), once made. */
  audio: string | null;
  /** The video with the translated track, for a video input. */
  video: string | null;
  elapsedMs: number;
  error: string | null;
  createdAt: number;
  startedAt: number | null;
  /** Transcribe: notes were asked for (in `summary`). */
  notes: boolean;
  length: Length;
  focus: string | null;
  /** Read aloud: a woman's voice. */
  female: boolean;
  /** The summary (Summarize) or the notes (Transcribe), as Markdown. */
  summary: string | null;
  /** Words read or heard. */
  words: number;
  /** Every file the run wrote (transcripts, subtitles, the summary, the reading). */
  files: string[];
}

/** A download a run still needs: what it is, in words, and its size. */
export interface Need {
  what: string;
  bytes: number;
}

/** What a run with these inputs would do and still needs (FlowService.Plan). */
export interface Plan {
  voiceName: string | null;
  cloned: boolean;
  /** One sentence: what will speak, or why nothing will. */
  spokenWith: string;
  /** Nothing speaks the language: the sentence is a warning, the run gives text and subtitles. */
  noVoice: boolean;
  needs: Need[];
  totalBytes: number;
  /** What else stands in the way (no file, no model to translate with), or null. */
  problem: string | null;
  ready: boolean;
  /** The chat model that translates. */
  modelId: string | null;
  modelName: string | null;
}

/** The downloads while they run: which one now, bytes so far of the whole, or what went wrong. */
export interface Install {
  what: string;
  done: number;
  total: number;
  error: string | null;
}

export interface Language {
  code: string;
  name: string;
  /** Whether a voice speaks it: only those are offered to translate into. */
  spoken?: boolean;
}

/** A change on the "flows" topic. */
export interface FlowsEvent {
  run?: Run;
  removed?: string;
  install?: Install | null;
}

/** The file types the Open dialog offers: what Nook reads itself, and what FFmpeg reads. */
export const MEDIA_EXTENSIONS = [
  "mp3", "m4a", "aac", "wav", "aiff", "aif", "flac", "ogg", "oga", "opus", "wma", "amr", "au", "caf",
  "mp4", "m4v", "mkv", "webm", "mov", "avi", "mpg", "mpeg", "wmv", "3gp", "ts",
];

export const flowsLanguages = () => call<Language[]>("flows_languages");
/** Every run, newest first. */
export const flowsRuns = () => call<Run[]>("flows_runs");

/** What a run would do and still needs, for a file (`input`) or the microphone. */
export const flowsPlan = (input: string | null, microphone: boolean, target: string, keepVoice: boolean) =>
  call<Plan>("flows_plan", { input, microphone, target, keepVoice });
/** Starts the one download of everything the plan says is missing; follow it with `onFlows`. */
export const flowsInstall = (input: string | null, microphone: boolean, target: string, keepVoice: boolean) =>
  call<void>("flows_install", { input, microphone, target, keepVoice });
export const flowsInstallState = () => call<Install | null>("flows_install_state");
export const flowsCancelInstall = () => call<void>("flows_cancel_install");
export const flowsClearInstallError = () => call<void>("flows_clear_install_error");

/** Queues the translation of a file; `source` null lets Whisper tell the language. */
export const flowsSubmit = (input: string, source: string | null, target: string, keepVoice: boolean) =>
  call<Run>("flows_submit", { input, source, target, keepVoice });
/** Opens the microphone; levels arrive on "speech". */
export const flowsRecordStart = () => call<void>("flows_record_start");
/** Stops recording and queues the translation of what was said. */
export const flowsRecordStop = (source: string | null, target: string, keepVoice: boolean) =>
  call<Run>("flows_record_stop", { source, target, keepVoice });
export const flowsRecordCancel = () => call<void>("flows_record_cancel");

export const flowsCancel = (id: string) => call<void>("flows_cancel", { id });
export const flowsDelete = (id: string) => call<boolean>("flows_delete", { id });
/** Run again / Try again: a file is read afresh, a recording copied into the new run. */
export const flowsAgain = (id: string) => call<Run>("flows_again", { id });
/** Opens a run's folder in Explorer, or the flows folder for null. */
export const flowsOpenFolder = (id: string | null) => call<void>("flows_open_folder", { id });
/** Opens a finished run's video (or its track) in the default player. */
export const flowsOpen = (id: string, video: boolean) => call<void>("flows_open", { id, video });
export const flowsReveal = (id: string) => call<void>("flows_reveal", { id });

/** What a Nooklet's run would do and still needs: a file, pasted `text`, or the microphone. */
export const flowsPlanFor = (input: string | null, microphone: boolean, text: string | null, order: Order) =>
  call<Plan>("flows_plan_for", { input, microphone, text, order });
export const flowsInstallFor = (input: string | null, microphone: boolean, text: string | null, order: Order) =>
  call<void>("flows_install_for", { input, microphone, text, order });
/** Queues a Nooklet's run on a file or pasted text. */
export const flowsSubmitFor = (input: string | null, text: string | null, order: Order) =>
  call<Run>("flows_submit_for", { input, text, order });
/** Stops recording and queues the transcript of what was said. */
export const flowsRecordStopFor = (order: Order) => call<Run>("flows_record_stop_for", { order });
/** A document's or text's language and words, before a run. */
export const flowsPeek = (input: string | null, text: string | null) => call<Peek>("flows_peek", { input, text });
/** Opens one of the files a run wrote. */
export const flowsOpenFile = (id: string, path: string) => call<void>("flows_open_file", { id, path });
export const flowsRevealFile = (id: string, path: string) => call<void>("flows_reveal_file", { id, path });

export const onFlows = (fn: (e: FlowsEvent) => void) => on<FlowsEvent>("flows", (p) => fn(p ?? {}));

/**
 * A URL the webview can play a run's file from: Tauri's asset protocol in the app (the flows folder
 * is in `assetProtocol.scope`), the path itself in the browser, where the mocks hand out blob: URLs.
 */
export function flowSrc(file: string): string {
  return inTauri ? convertFileSrc(file) : file;
}

// ---------------------------------------------------------------- Run's derived values

export function runFinished(r: Run): boolean {
  return r.status === "DONE" || r.status === "FAILED" || r.status === "CANCELLED";
}

/**
 * Run.progress(): rough share of the work done, 0 to 1: for a translation, converting is quick,
 * listening about a quarter of the rest on a GPU, translating another, speaking the other half;
 * the Nooklets' shares are their own.
 */
export function runProgress(r: Run): number {
  if (r.status === "DONE") return 1;
  if (r.status !== "RUNNING" || r.stage == null) return 0;
  const within = r.total > 0 ? Math.min(1, r.done / r.total) : 0;
  if (r.flow === TRANSCRIBE && r.stage === "LISTENING") return 0.05 + (r.notes ? 0.65 : 0.9) * within;
  if (r.flow === TRANSCRIBE && r.stage === "SUMMARIZING") return 0.7 + 0.27 * within;
  if (r.flow === SUMMARIZE && r.stage === "SUMMARIZING") return 0.1 + 0.87 * within;
  if (r.flow === READ_ALOUD && r.stage === "READING") return 0.03;
  if (r.flow === READ_ALOUD && r.stage === "SPEAKING") return 0.06 + 0.86 * within;
  if (r.flow === READ_ALOUD && r.stage === "ASSEMBLING") return 0.94;
  switch (r.stage) {
    case "READING":
      return 0.05;
    case "SUMMARIZING":
      return 0.3 + 0.6 * within;
    case "PREPARING":
      return 0.03;
    case "LISTENING":
      return 0.05 + 0.25 * within;
    case "TRANSLATING":
      return 0.3 + 0.25 * within;
    case "SPEAKING":
      return 0.55 + 0.4 * within;
    case "ASSEMBLING":
      return 0.96;
    case "SAVING":
      return 0.99;
  }
}
