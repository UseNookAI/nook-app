/**
 * The screen recorder Nooklet (src-tauri/src/commands/capture.rs): the screens, windows and sound
 * devices there are, a picture of what a source records, the sound's meters, recording and
 * streaming, FFmpeg's download, stream keys kept for this Windows account, and the recorder's own
 * windows (the area picker and the recording controls). Its news comes on the "capture" topic:
 * `{state}`, `{levels}` (each sound's loudness, 0 to 1, ten times a second), `{install}` and
 * `{area}` (what the picker chose, or null).
 */
import type { Install } from "./flows";
import { call, inTauri, on } from "./ipc";

export interface Screen {
  /** Windows' handle for it, which recording takes. */
  handle: number;
  /** "Screen 1". */
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  primary: boolean;
}

export interface CaptureWindow {
  handle: number;
  title: string;
  /** The program: "chrome", "Code". */
  app: string;
  width: number;
  height: number;
}

export interface AudioDevice {
  name: string;
  default: boolean;
}

export interface Sources {
  screens: Screen[];
  windows: CaptureWindow[];
  microphones: AudioDevice[];
  speakers: AudioDevice[];
  /** FFmpeg is in; else it downloads first (`downloadBytes`). */
  ready: boolean;
  downloadBytes: number;
  /** Where recordings go unless another folder is chosen. */
  folder: string;
}

export type Source =
  | { kind: "screen"; handle: number }
  | { kind: "window"; handle: number }
  | { kind: "area"; screen: number; x: number; y: number; width: number; height: number };

/** A sound to record; `device` null is Windows' default. */
export type AudioSource = { kind: "microphone"; device: string | null } | { kind: "system"; device: string | null };

export type Quality = "standard" | "high";

export interface StreamOptions {
  /** "rtmp://live.twitch.tv/app". */
  server: string;
  key: string;
  kbps: number;
}

export interface StartOptions {
  source: Source;
  fps: number;
  /** Lines to scale down to (1080, 720), or null for the source's own size. */
  scaleTo: number | null;
  quality: Quality;
  cursor: boolean;
  audio: AudioSource[];
  record: boolean;
  folder: string | null;
  stream: StreamOptions | null;
}

export type Phase = "idle" | "starting" | "live" | "paused" | "finishing";

export interface Saved {
  path: string;
  bytes: number;
  seconds: number;
}

export interface CaptureState {
  phase: Phase;
  recording: boolean;
  streaming: boolean;
  /** Time recorded, pauses left out. */
  seconds: number;
  bytes: number;
  fps: number;
  kbps: number;
  dropped: number;
  /** What encodes: "NVIDIA graphics card". */
  encoder: string | null;
  width: number;
  height: number;
  monitor: number | null;
  /** Why the last one ended or did not start. */
  error: string | null;
  /** The stream stopped while the recording goes on. */
  streamError: string | null;
  saved: Saved | null;
}

export type CaptureEvent = {
  state?: CaptureState;
  levels?: number[];
  install?: Install | null;
  area?: Source | null;
};

export const IDLE: CaptureState = {
  phase: "idle",
  recording: false,
  streaming: false,
  seconds: 0,
  bytes: 0,
  fps: 0,
  kbps: 0,
  dropped: 0,
  encoder: null,
  width: 0,
  height: 0,
  monitor: null,
  error: null,
  streamError: null,
  saved: null,
};

/** The streaming services Nook knows the servers of; "custom" takes any address. */
export interface StreamService {
  id: "twitch" | "youtube" | "facebook" | "kick" | "custom";
  name: string;
  server: string;
  /** The most the service takes, kbit/s. */
  maxKbps: number;
  /** Where the person finds their key. */
  keyHelp: string;
}

export const SERVICES: StreamService[] = [
  {
    id: "twitch",
    name: "Twitch",
    server: "rtmp://live.twitch.tv/app",
    maxKbps: 6000,
    keyHelp: "Twitch: Creator Dashboard › Settings › Stream › Primary Stream key",
  },
  {
    id: "youtube",
    name: "YouTube",
    server: "rtmp://a.rtmp.youtube.com/live2",
    maxKbps: 12000,
    keyHelp: "YouTube Studio › Create › Go live › Stream › Stream key",
  },
  {
    id: "facebook",
    name: "Facebook",
    server: "rtmps://live-api-s.facebook.com:443/rtmp",
    maxKbps: 8000,
    keyHelp: "Facebook › Live video › Streaming software › Stream key",
  },
  {
    id: "kick",
    name: "Kick",
    server: "",
    maxKbps: 8000,
    keyHelp: "Kick › Creator Dashboard › Settings › Stream URL & Key: paste both",
  },
  {
    id: "custom",
    name: "Another service",
    server: "",
    maxKbps: 50000,
    keyHelp: "Any RTMP, RTMPS or SRT address, with its stream key",
  },
];

export const captureSources = () => call<Sources>("capture_sources");
export const capturePreview = (source: Source) => call<string>("capture_preview", { source });
export const captureListen = (audio: AudioSource[]) => call<void>("capture_listen", { audio });
export const captureStopListening = () => call<void>("capture_stop_listening");
export const captureState = () => call<CaptureState>("capture_state");
export const captureStart = (options: StartOptions, hide: boolean) => call<CaptureState>("capture_start", { options, hide });
export const capturePause = () => call<CaptureState>("capture_pause");
export const captureResume = () => call<CaptureState>("capture_resume");
export const captureStop = () => call<CaptureState>("capture_stop");
export const capturePickArea = () => call<void>("capture_pick_area");
export const captureAreaPicked = (area: Source | null) => call<void>("capture_area_picked", { area });
export const captureInstall = () => call<void>("capture_install");
export const captureInstallState = () => call<Install | null>("capture_install_state");
export const captureCancelInstall = () => call<void>("capture_cancel_install");
export const captureClearInstallError = () => call<void>("capture_clear_install_error");
export const captureStreamKey = (service: string) => call<string | null>("capture_stream_key", { service });
export const captureKeepStreamKey = (service: string, key: string | null) => call<void>("capture_keep_stream_key", { service, key });
export const captureOpen = (path: string) => call<void>("capture_open", { path });
export const captureReveal = (path: string) => call<void>("capture_reveal", { path });
export const captureOpenFolder = (folder: string | null) => call<void>("capture_open_folder", { folder });
export const onCapture = (fn: (e: CaptureEvent) => void) => on<CaptureEvent>("capture", (p) => fn(p ?? {}));

/** A folder for recordings, chosen in Windows' folder picker; null when cancelled. */
export async function chooseRecordingFolder(): Promise<string | null> {
  if (!inTauri) return "C:\\Users\\you\\Videos\\Recordings";
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ directory: true, multiple: false, title: "Save recordings in" });
  return typeof picked === "string" ? picked : null;
}

/** "3:07", "1:02:45". */
export function clock(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/** What a source is, in a few words: "Screen 1", "chrome — Inbox", "An area of Screen 1, 1280 × 720". */
export function sourceText(source: Source | null, sources: Sources | null): string {
  if (!source) return "Nothing chosen";
  switch (source.kind) {
    case "screen":
      return sources?.screens.find((s) => s.handle === source.handle)?.name ?? "A screen";
    case "window": {
      const w = sources?.windows.find((x) => x.handle === source.handle);
      return w ? w.title : "A window";
    }
    case "area": {
      const s = sources?.screens.find((x) => x.handle === source.screen);
      return `An area of ${s?.name ?? "a screen"}, ${source.width} × ${source.height}`;
    }
  }
}
