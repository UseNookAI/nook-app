/**
 * Browser stand-ins for the screen recorder: two screens, a handful of windows, a headset and
 * speakers, meters that move, and a recording that counts up as FFmpeg's progress would. FFmpeg
 * starts not downloaded; `?captureMock=ready` starts with it in. Picking an area answers with one
 * in the middle of Screen 1 after a moment. A stream key with "bad" in it is refused as a
 * service would.
 */
import type { AudioSource, CaptureState, Sources, StartOptions } from "../capture";
import { IDLE } from "../capture";
import type { Install } from "../flows";
import { mock, mockEmit } from "../ipc";

let ready = new URLSearchParams(window.location.search).get("captureMock")?.includes("ready") ?? false;
let install: Install | null = null;
let installTimer: number | null = null;
let meter: number | null = null;
let state: CaptureState = { ...IDLE };
let ticker: number | null = null;
const keys = new Map<string, string>();
const FFMPEG_BYTES = 80_726_424;

const SOURCES = (): Sources => ({
  screens: [
    { handle: 65537, name: "Screen 1", x: 0, y: 0, width: 2560, height: 1440, primary: true },
    { handle: 65539, name: "Screen 2", x: 2560, y: 0, width: 1920, height: 1080, primary: false },
  ],
  windows: [
    { handle: 1001, title: "Quarterly planning - Google Chrome", app: "chrome", width: 1600, height: 1000 },
    { handle: 1002, title: "main.rs - nook-rs - Visual Studio Code", app: "Code", width: 1920, height: 1040 },
    { handle: 1003, title: "Figma - Onboarding flow", app: "Figma", width: 1440, height: 900 },
    { handle: 1004, title: "Spotify Premium", app: "Spotify", width: 1200, height: 760 },
  ],
  microphones: [
    { name: "Headset Microphone (HyperX Cloud II)", default: true },
    { name: "Microphone Array (Realtek Audio)", default: false },
  ],
  speakers: [
    { name: "Headset Earphone (HyperX Cloud II)", default: true },
    { name: "Speakers (Realtek Audio)", default: false },
  ],
  ready,
  downloadBytes: FFMPEG_BYTES,
  folder: "C:\\Users\\you\\Videos\\Nook",
});

function emitState(next: CaptureState) {
  state = next;
  mockEmit("capture", { state });
}

/** A picture standing in for what a source shows: its name on a coloured card. */
function picture(label: string, w: number, h: number): string {
  const ratio = w / h;
  const width = 480;
  const height = Math.round(width / ratio);
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">
<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#1e3a5f"/><stop offset="1" stop-color="#3b6fa6"/></linearGradient></defs>
<rect width="100%" height="100%" fill="url(#g)"/>
<rect x="24" y="24" width="${width - 48}" height="28" rx="6" fill="#ffffff22"/>
<rect x="24" y="68" width="${(width - 48) * 0.55}" height="${height - 110}" rx="8" fill="#ffffff18"/>
<rect x="${24 + (width - 48) * 0.58}" y="68" width="${(width - 48) * 0.42}" height="${(height - 110) / 2 - 6}" rx="8" fill="#ffffff18"/>
<text x="50%" y="${height - 16}" fill="#ffffffcc" font-family="Segoe UI, sans-serif" font-size="14" text-anchor="middle">${label.replace(/[<&]/g, "")}</text>
</svg>`;
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}

function stopTicker() {
  if (ticker != null) window.clearInterval(ticker);
  ticker = null;
}

export function registerCaptureMocks(): void {
  mock("capture_sources", async () => {
    await new Promise((r) => setTimeout(r, 250));
    return SOURCES();
  });
  mock("capture_preview", async (a) => {
    await new Promise((r) => setTimeout(r, 450));
    if (!ready) throw new Error("FFmpeg is not installed yet.");
    const source = a.source as { kind: string; handle?: number; width?: number; height?: number };
    const s = SOURCES();
    if (source.kind === "window") {
      const w = s.windows.find((x) => x.handle === source.handle);
      return picture(w?.title ?? "A window", w?.width ?? 1600, w?.height ?? 900);
    }
    if (source.kind === "area") return picture("An area", source.width ?? 1280, source.height ?? 720);
    const sc = s.screens.find((x) => x.handle === source.handle) ?? s.screens[0];
    return picture(sc.name, sc.width, sc.height);
  });
  mock("capture_listen", (a) => {
    if (meter != null) window.clearInterval(meter);
    const audio = (a.audio as AudioSource[]) ?? [];
    if (audio.length === 0) return;
    let t = 0;
    meter = window.setInterval(() => {
      t += 1;
      mockEmit("capture", {
        levels: audio.map((s, i) => (s.kind === "microphone" ? Math.abs(Math.sin(t / 3)) * 0.55 : 0.25 + 0.2 * Math.sin(t / 5 + i))),
      });
    }, 100);
  });
  mock("capture_stop_listening", () => {
    if (meter != null) window.clearInterval(meter);
    meter = null;
  });
  mock("capture_state", () => state);
  mock("capture_start", async (a) => {
    const options = a.options as StartOptions;
    if (!ready) throw new Error("FFmpeg is not installed yet: download it first.");
    if (options.stream && options.stream.key.includes("bad"))
      throw new Error("The streaming service did not take the stream: check its server address and stream key. (Server error: Invalid stream key)");
    emitState({ ...IDLE, phase: "starting" });
    await new Promise((r) => setTimeout(r, 900));
    const width = options.scaleTo ? Math.round((options.scaleTo * 16) / 9) : 2560;
    const height = options.scaleTo ?? 1440;
    emitState({
      ...IDLE,
      phase: "live",
      recording: options.record,
      streaming: !!options.stream,
      encoder: "NVIDIA graphics card",
      width,
      height,
      monitor: 65537,
    });
    stopTicker();
    ticker = window.setInterval(() => {
      if (state.phase !== "live") return;
      emitState({ ...state, seconds: state.seconds + 0.5, bytes: state.bytes + 350_000, fps: options.fps, kbps: 5600 });
    }, 500);
    return state;
  });
  mock("capture_pause", () => {
    emitState({ ...state, phase: "paused", fps: 0, kbps: 0 });
    return state;
  });
  mock("capture_resume", () => {
    emitState({ ...state, phase: "live" });
    return state;
  });
  mock("capture_stop", async () => {
    if (state.phase === "idle") return state;
    stopTicker();
    const recorded = state.recording;
    emitState({ ...state, phase: "finishing" });
    await new Promise((r) => setTimeout(r, 700));
    emitState({
      ...IDLE,
      seconds: state.seconds,
      saved: recorded
        ? { path: "C:\\Users\\you\\Videos\\Nook\\Screen recording 2026-09-29 at 14.03.22.mp4", bytes: state.bytes, seconds: state.seconds }
        : null,
    });
    return state;
  });
  mock("capture_pick_area", () => {
    window.setTimeout(
      () => mockEmit("capture", { area: { kind: "area", screen: 65537, x: 640, y: 360, width: 1280, height: 720 } }),
      900,
    );
  });
  mock("capture_area_picked", (a) => mockEmit("capture", { area: a.area ?? null }));
  mock("capture_install", () => {
    if (ready || (install && !install.error)) return;
    install = { what: "FFmpeg", done: 0, total: FFMPEG_BYTES, error: null };
    mockEmit("capture", { install });
    installTimer = window.setInterval(() => {
      if (!install) return;
      install = { ...install, done: Math.min(install.total, install.done + 9_000_000) };
      if (install.done >= install.total) {
        if (installTimer != null) window.clearInterval(installTimer);
        install = null;
        ready = true;
      }
      mockEmit("capture", { install });
    }, 250);
  });
  mock("capture_install_state", () => install);
  mock("capture_cancel_install", () => {
    if (installTimer != null) window.clearInterval(installTimer);
    if (install) install = { ...install, error: "The download was stopped." };
    mockEmit("capture", { install });
  });
  mock("capture_clear_install_error", () => {
    install = null;
    mockEmit("capture", { install });
  });
  mock("capture_stream_key", (a) => keys.get(a.service as string) ?? null);
  mock("capture_keep_stream_key", (a) => {
    const key = (a.key as string | null)?.trim();
    if (key) keys.set(a.service as string, key);
    else keys.delete(a.service as string);
  });
  mock("capture_open", () => undefined);
  mock("capture_reveal", () => undefined);
  mock("capture_open_folder", () => undefined);
}
