/**
 * The video tool (nook_core::video, src-tauri/src/commands/video.rs). Types mirror the Java of
 * `ai.nook.agent.video.VideoStudio` (Clip, Status) and `ai.nook.agent.runtime.VideoEngine`
 * (Stage) field for field; instants are epoch milliseconds, paths absolute strings.
 *
 * Events: the topic "video" fires whenever a clip is added, moves on (at most every 250 ms inside
 * a stage), finishes or is removed (VideoStudio's listeners); the payload is not used, the page
 * re-reads `video_clips`. The topic "downloads" (ModelDownloadService) makes the page re-read its
 * setup and the video model's download state.
 */
import { convertFileSrc } from "@tauri-apps/api/core";
import { call, inTauri } from "./ipc";

/** VideoStudio.Status. */
export type Status = "QUEUED" | "RUNNING" | "DONE" | "FAILED" | "CANCELLED";

/** VideoEngine.Stage: what the engine is doing while a clip is RUNNING. */
export type Stage = "LOADING" | "SAMPLING" | "DECODING" | "SAVING";

/** VideoStudio.Clip: one clip, asked for, being made or made. The studio replaces it as it moves on. */
export interface Clip {
  id: string;
  prompt: string;
  modelId: string | null;
  status: Status;
  /** What the engine is doing while RUNNING, else null. */
  stage: Stage | null;
  /** Units finished in the stage (sampling steps), with `total`; 0 when unknown. */
  done: number;
  total: number;
  /** The finished video (`<videos>\<id>.avi`, MJPEG) while DONE, else null. */
  file: string | null;
  width: number;
  height: number;
  frames: number;
  fps: number;
  seed: number;
  elapsedMs: number;
  error: string | null;
  /** Epoch ms. */
  createdAt: number;
  /** Epoch ms when the engine took the clip; null while it waits. */
  startedAt: number | null;
}

/** An installed video model a clip can be made with (RuntimeManager.videoModel candidates). */
export interface VideoModel {
  id: string;
  name: string;
}

/**
 * What the page needs to know before a clip can be asked for (VideoScreen.kt `readSetup`): whether
 * the model and engine are in, which model makes the clips, and what they come out as.
 */
export interface VideoSetup {
  /** Why no clip can be made yet (RuntimeManager.videoProblem), or null. */
  problem: string | null;
  /** The catalog id of the model: the installed one asked for or preferred, else the catalog default. */
  modelId: string | null;
  /** Its display name, or "No video model". */
  modelName: string;
  /** What the setup card offers to download: the model when missing, plus the sd engine when missing. */
  downloadBytes: number;
  /** The model's clip defaults (catalog `defaults`, else 832, 480, 33 and 16). */
  width: number;
  height: number;
  frames: number;
  fps: number;
  /** Every installed video model, for the choice in the header (shown when there are several). */
  models: VideoModel[];
  /** Where finished clips are kept (VideoStudio.folder()). */
  folder: string;
}

/** Where the video model's download is, for the setup card (ModelDownloadService state). */
export interface DownloadState {
  /** The catalog lists the model, so there is something to download. */
  offered: boolean;
  downloading: boolean;
  paused: boolean;
  /** 0..1, or null before the first progress report. */
  progress: number | null;
}

/** VideoStudio.clips(): every clip, newest first. */
export const videoClips = () => call<Clip[]>("video_clips");

/**
 * VideoStudio.submit(prompt, modelId): queues a clip; it starts when the clips before it are done.
 * Rejects with "Write what the video should show." or the setup problem. `modelId` null = the
 * preferred installed video model.
 */
export const videoSubmit = (prompt: string, modelId: string | null) => call<Clip>("video_submit", { prompt, modelId });

/** VideoStudio.cancel(id): stops a queued or running clip; a finished one is left as it is. */
export const videoCancel = (id: string) => call<void>("video_cancel", { id });

/** VideoStudio.delete(id): removes a finished clip and its files. False for an unknown or unfinished clip. */
export const videoDelete = (id: string) => call<boolean>("video_delete", { id });

/**
 * VideoScreen.kt readSetup + VideoStudio.problem() + folder(): the setup for the model a clip would
 * use (RuntimeManager.videoModel(modelId); null = the preferred one).
 */
export const videoSetup = (modelId: string | null) => call<VideoSetup>("video_setup", { modelId });

/** ModelDownloadService availableModels / downloadingModels / pausedModels / downloadingProgress for one catalog model. */
export const videoDownloadState = (modelId: string) => call<DownloadState>("video_download_state", { modelId });

/** ModelDownloadService.launchDownload: the model's files, and the sd engine when it is missing. */
export const videoDownload = (modelId: string) => call<void>("video_download", { modelId });
/** ModelDownloadService.pauseDownload. */
export const videoDownloadPause = (modelId: string) => call<void>("video_download_pause", { modelId });
/** ModelDownloadService.resumeDownload. */
export const videoDownloadResume = (modelId: string) => call<void>("video_download_resume", { modelId });

/** Creates the videos folder when needed and opens it in Explorer (VideoScreen.kt openFolder). */
export const videoOpenFolder = () => call<void>("video_open_folder");
/** Opens a finished clip in the default video player (Desktop.open(clip.file())). */
export const videoOpen = (id: string) => call<void>("video_open", { id });
/** Shows a finished clip selected in Explorer (`explorer.exe /select, <file>`). */
export const videoReveal = (id: string) => call<void>("video_reveal", { id });

/**
 * A URL the webview can fetch a clip file from: Tauri's asset protocol in the app (the videos
 * folder must be in `assetProtocol.scope`), the path itself in the browser, where the mocks hand
 * out blob: URLs as paths.
 */
export function clipSrc(file: string): string {
  return inTauri ? convertFileSrc(file) : file;
}

// ---------------------------------------------------------------- Clip's derived values

/** Clip.seconds(): the clip's length. */
export function clipSeconds(c: Clip): number {
  return c.fps > 0 ? c.frames / c.fps : 0;
}

/** Clip.finished(). */
export function clipFinished(c: Clip): boolean {
  return c.status === "DONE" || c.status === "FAILED" || c.status === "CANCELLED";
}

/**
 * Clip.progress(): rough share of the work done, 0 to 1, for a progress bar. The shares are a Wan
 * 2.1 clip on an RTX 4060 (2026-09-24): weights and prompt 27 s, 20 sampling steps 202 s, decoding
 * the frames tile by tile 80 s.
 */
export function clipProgress(c: Clip): number {
  if (c.status === "DONE") return 1;
  if (c.status !== "RUNNING" || c.stage == null) return 0;
  const inStage = c.total > 0 ? Math.min(1, c.done / c.total) : 0;
  switch (c.stage) {
    case "LOADING":
      return 0.09 * inStage;
    case "SAMPLING":
      return 0.09 + 0.65 * inStage;
    case "DECODING":
      return 0.74 + 0.25 * inStage;
    case "SAVING":
      return 0.99;
  }
}
