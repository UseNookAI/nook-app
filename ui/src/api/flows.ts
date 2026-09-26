/**
 * The Flows page (nook_core::flow, src-tauri/src/commands/flows.rs). Types mirror the Rust of
 * `FlowService` (Run, Status, Stage, Source, Plan, Need, Install) and `flow::subtitles::Segment`
 * field for field; instants are epoch milliseconds, paths absolute strings.
 *
 * Events on the topic "flows": `{ run }` when a run is added or moves on, `{ removed: id }` when one
 * is deleted, `{ install }` (an Install or null) while the downloads run. The topic "downloads"
 * (the runtime's) and "runtime" make the page read its plan again. Microphone levels arrive on
 * "speech", as for the voice prompt (`onSpeechLevel` in ./code).
 */
import { convertFileSrc } from "@tauri-apps/api/core";
import { call, inTauri, on } from "./ipc";

/** FlowService.Status. */
export type Status = "QUEUED" | "RUNNING" | "DONE" | "FAILED" | "CANCELLED";

/** FlowService.Stage: what a RUNNING translation is doing. */
export type Stage = "PREPARING" | "LISTENING" | "TRANSLATING" | "SPEAKING" | "ASSEMBLING" | "SAVING";

/** Where a run's sound came from: a file, or the microphone (its recording is in the run's folder). */
export type Source = "FILE" | "MICROPHONE";

/** One stretch of speech: where it is in the track (seconds), what was said, and its translation once made. */
export interface Segment {
  start: number;
  end: number;
  text: string;
  translation: string | null;
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
 * Run.progress(): rough share of the work done, 0 to 1: converting is quick, listening about a
 * quarter of the rest on a GPU, translating another, speaking the other half.
 */
export function runProgress(r: Run): number {
  if (r.status === "DONE") return 1;
  if (r.status !== "RUNNING" || r.stage == null) return 0;
  const within = r.total > 0 ? Math.min(1, r.done / r.total) : 0;
  switch (r.stage) {
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
