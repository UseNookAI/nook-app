/**
 * Code sessions (nook_core::code, src-tauri/src/commands/code.rs). Types mirror the Java records
 * of `ai.nook.agent.code` (CodeSession, CodeService) field for field.
 */
import { call, on } from "./ipc";

/** What the scratch copy holds that the repository does not: the diff (cut for the record), its stat. */
export interface Change {
  diff: string;
  cut: boolean;
  stat: string;
}

/** How full the worker's context was in a run. */
export interface RunContext {
  used: number;
  peak: number;
  window: number;
  dropped: number;
  measured: boolean;
}

export interface Task {
  kind: "task";
  id: string;
  at: number;
  text: string;
}

export interface Run {
  kind: "run";
  id: string;
  at: number;
  model: string | null;
  running: boolean;
  steps: string[];
  summary: string | null;
  stat: string | null;
  verified: boolean | null;
  verifyCommand: string | null;
  verifyNote: string | null;
  verifyOutput: string | null;
  gaveUp: string | null;
  toolCalls: number;
  seconds: number;
  error: string | null;
  before: string | null;
  undone: boolean;
  after: string | null;
  context: RunContext | null;
}

/** Tones: "applied" and "discarded" end a change; "undone", "info", "ok", "error" are said only. */
export interface Note {
  kind: "note";
  id: string;
  at: number;
  text: string;
  tone: string | null;
}

export type Entry = Task | Run | Note;

export interface CodeSession {
  id: string;
  title: string;
  repository: string;
  createdAt: number;
  updatedAt: number;
  worktree: string | null;
  baseCommit: string | null;
  baseline: string | null;
  verify: string | null;
  change: Change | null;
  entries: Entry[];
  /** The page it was started from, whose sidebar lists it: the Chat page or the Code page's Nook panel. */
  origin?: SessionOrigin;
}

export type SessionOrigin = "chat" | "editor";

/** Whether a session belongs to the Code page's Nook panel (an older one without an origin is the Chat page's). */
export const isEditorSession = (s: CodeSession) => s.origin === "editor";

export interface WorkerChoice {
  id: string;
  name: string;
  tested: boolean;
}

/** What Code mode shows (CodeHub.CodeSnapshot), re-read whenever a "code" event arrives. */
export interface CodeSnapshot {
  sessions: CodeSession[];
  workerName: string | null;
  workerHint: string;
  workers: WorkerChoice[];
  workerId: string | null;
  /** What each running turn is doing now, by run id. "thinking" while the model thinks. */
  phases: Record<string, string>;
}

export interface NextContext {
  tokens: number;
  recap: number;
  window: number;
}

export type Readiness = "REPOSITORY" | "FOLDER" | "MISSING" | "TOO_BIG";

export interface RepositoryState {
  readiness: Readiness;
  root: string | null;
  reason: string | null;
}

export interface SpeechModel {
  id: string;
  name: string;
  bytes: number;
}

/** Which file is open in the Code page and what is selected, sent with a request from its panel. */
export interface EditorContext {
  file: string | null;
  selection: string | null;
  /** 1-based line range of the selection, when there is one. */
  lines: [number, number] | null;
}

export const THINKING = "thinking";

export const codeSnapshot = () => call<CodeSnapshot>("code_snapshot");
export const codeStart = (
  folder: string,
  text: string,
  verify: string | null = null,
  context: EditorContext | null = null,
  origin: SessionOrigin = "chat",
) => call<CodeSession>("code_start", { folder, text, verify, context, origin });
export const codeSend = (id: string, text: string, verify: string | null = null, context: EditorContext | null = null) =>
  call<void>("code_send", { id, text, verify, context });
export const codeStop = (id: string) => call<void>("code_stop", { id });
export const codeApply = (id: string) => call<void>("code_apply", { id });
export const codeDiscard = (id: string) => call<void>("code_discard", { id });
export const codeUndo = (id: string) => call<void>("code_undo", { id });
export const codeDelete = (id: string) => call<void>("code_delete", { id });
export const codeRename = (id: string, title: string) => call<void>("code_rename", { id, title });
/** The code a run wrote (its diff), or null. */
export const codeRunDiff = (sessionId: string, runId: string) => call<string | null>("code_run_diff", { sessionId, runId });
/** What the next request of a session starts with; null without a session or a worker. */
export const codeNextContext = (id: string) => call<NextContext | null>("code_next_context", { id });
export const codeRepositoryState = (folder: string) => call<RepositoryState>("code_repository_state", { folder });
export const codeRecentRepositories = () => call<string[]>("code_recent_repositories");
export const codeSetWorker = (modelId: string) => call<void>("code_set_worker", { modelId });
export const codeSpeechProblem = () => call<string | null>("code_speech_problem");
export const codeSpeechModel = () => call<SpeechModel | null>("code_speech_model");
/** Starts the microphone (nook_core::speech); levels arrive as "speech" events. */
export const speechStart = () => call<void>("speech_start");
/** Stops recording and transcribes it with the speech model. */
export const speechStopAndTranscribe = () => call<string>("speech_stop_and_transcribe");
export const speechCancel = () => call<void>("speech_cancel");

/**
 * The speech model's download, for the voice-input install card (CodeModels.SpeechInstall, which
 * read ModelDownloadService for that one model): whether the catalog offers it, whether it is
 * downloading, and how far (0..1, null before the first byte).
 */
export interface SpeechDownload {
  available: boolean;
  downloading: boolean;
  progress: number | null;
}

export const codeSpeechDownload = () => call<SpeechDownload>("code_speech_download");
/** Starts downloading the speech model (codeSpeechModel) and, with it, the speech engine. */
export const codeSpeechInstall = () => call<void>("code_speech_install");

/** Calls back whenever sessions or phases change. */
export const onCodeChanged = (fn: () => void) => on<unknown>("code", fn);
/**
 * Calls back when downloads move or finish ("downloads", ModelDownloadService and the runtime):
 * a model that finishes downloading joins the worker menu (CodeHub.kt read the snapshot again
 * whenever the installed models changed).
 */
export const onDownloadsChanged = (fn: () => void) => on<unknown>("downloads", fn);
/**
 * Calls back with each microphone level (0..1) while speechStart records. The "speech" topic also
 * carries `{ kind: "started" | "stopped" | "cancelled" | "error" }` (nook_core::speech::SpeechEvent;
 * a level is `{ kind: "level", level }`); only the levels are passed on.
 */
export const onSpeechLevel = (fn: (level: number) => void) =>
  on<{ kind?: string; level?: number }>("speech", (p) => {
    if (typeof p?.level === "number") fn(p.level);
  });
