/**
 * Browser stand-ins for the video commands (VideoStudio, the setup, the model download). The
 * finished clips are real MJPEG AVIs, drawn frame by frame on a canvas and wrapped in a RIFF
 * container here, so the page's own player plays them. A running clip moves through the stages
 * on a timer and the queue runs one clip at a time, as the studio's single worker does.
 *
 * States to look at: `?videoMock=no-model` (or `no-engine`, `not-offered`, `empty`, `two-models`,
 * comma-separated) in the URL, or `nookVideoMock.setup("no-model")`, `nookVideoMock.twoModels()`,
 * `nookVideoMock.clear()`, `nookVideoMock.broken()` in the console.
 */
import { mock, mockEmit } from "../ipc";
import type { Clip, DownloadState, Stage, Status, VideoModel, VideoSetup } from "../video";

type SetupState = "ready" | "no-model" | "no-engine" | "not-offered";

const MODEL: VideoModel = { id: "wan2.1-t2v-1.3b", name: "Wan 2.1 T2V 1.3B" };
const OTHER_MODEL: VideoModel = { id: "wan2.2-ti2v-5b", name: "Wan 2.2 TI2V 5B" };
/** The catalog's three Wan 2.1 files, and the CUDA sd engine with its runtime (engines.json). */
const MODEL_BYTES = 1_535_768_800 + 4_145_878_880 + 253_815_318;
const ENGINE_BYTES = 336_399_103 + 563_500_000;
const FOLDER = "C:\\Users\\you\\AppData\\Local\\Nook-rs\\videos";
const NO_MODEL = "No video model is downloaded yet.";
const NO_ENGINE = "The video engine is not installed yet. Download the video model to install it.";

let setupState: SetupState = "ready";
let twoModels = false;
let download: DownloadState = { offered: true, downloading: false, paused: false, progress: null };
let downloadTimer: number | undefined;

let clips: Clip[] = [];
const stopRequested = new Set<string>();
const finishing = new Set<string>();
let stageAt = 0;
let ticker: number | undefined;

const changed = () => mockEmit("video", null);
const downloadsChanged = () => mockEmit("downloads", null);

function problem(): string | null {
  if (setupState === "ready") return null;
  return setupState === "no-engine" ? NO_ENGINE : NO_MODEL;
}

/** Sortable by time and unique, as VideoStudio.newId. */
function newId(): string {
  return "vid_" + Date.now().toString(36) + Math.random().toString(16).slice(2, 8);
}

function blank(over: Partial<Clip> & Pick<Clip, "id" | "prompt" | "status" | "createdAt">): Clip {
  return {
    modelId: MODEL.id,
    stage: null,
    done: 0,
    total: 0,
    file: null,
    width: 0,
    height: 0,
    frames: 0,
    fps: 0,
    seed: 0,
    elapsedMs: 0,
    error: null,
    startedAt: null,
    ...over,
  };
}

const put = (c: Clip) => {
  clips = clips.map((x) => (x.id === c.id ? c : x));
};

/** VideoStudio.failed: a clip that stopped, without its render details. */
const stopped = (c: Clip, status: Status, error: string | null): Clip =>
  blank({ id: c.id, prompt: c.prompt, modelId: c.modelId, status, error, createdAt: c.createdAt, startedAt: c.startedAt });

// ---------------------------------------------------------------- the worker

function move(c: Clip, stage: Stage, done: number, total: number) {
  put({ ...c, stage, done, total });
  stageAt = Date.now();
  changed();
}

function step(c: Clip, done: number) {
  put({ ...c, done });
  changed();
}

function tick() {
  const now = Date.now();
  const r = clips.find((c) => c.status === "RUNNING");
  if (!r) {
    const next = clips.filter((c) => c.status === "QUEUED").sort((a, b) => a.createdAt - b.createdAt)[0];
    if (!next) return;
    put({ ...next, status: "RUNNING", stage: "LOADING", done: 0, total: 0, startedAt: now });
    stageAt = now;
    changed();
    return;
  }
  if (stopRequested.has(r.id)) {
    stopRequested.delete(r.id);
    put(stopped(r, "CANCELLED", null));
    changed();
    return;
  }
  const inStage = now - stageAt;
  switch (r.stage) {
    case "LOADING": {
      // RuntimeManager.generateVideo checks the setup again before it starts the engine.
      const p = problem();
      if (p) {
        put(stopped(r, "FAILED", p));
        changed();
      } else if (inStage >= 3000) move(r, "SAMPLING", 0, 20);
      break;
    }
    case "SAMPLING": {
      // A sampling step every 0.7 s, then the frames in 8 tiles.
      const d = Math.floor(inStage / 700);
      if (d >= r.total) move(r, "DECODING", 0, 8);
      else if (d !== r.done) step(r, d);
      break;
    }
    case "DECODING": {
      const d = Math.floor(inStage / 350);
      if (d >= r.total) move(r, "SAVING", 0, 0);
      else if (d !== r.done) step(r, d);
      break;
    }
    case "SAVING":
      if (inStage >= 800 && !finishing.has(r.id)) void finish(r);
      break;
  }
}

async function finish(r: Clip) {
  finishing.add(r.id);
  try {
    const file = await makeAvi(sceneFor(r.prompt));
    const now = clips.find((c) => c.id === r.id);
    if (!now || now.status !== "RUNNING") {
      URL.revokeObjectURL(file);
      return;
    }
    put({
      ...stopped(now, "DONE", null),
      file,
      width: 832,
      height: 480,
      frames: 33,
      fps: 16,
      seed: Math.floor(Math.random() * 2_000_000_000),
      elapsedMs: Date.now() - (now.startedAt ?? Date.now()),
    });
    changed();
  } finally {
    finishing.delete(r.id);
  }
}

// ---------------------------------------------------------------- the download

function runDownload() {
  window.clearInterval(downloadTimer);
  download = { ...download, downloading: true, paused: false, progress: download.progress ?? 0 };
  downloadsChanged();
  downloadTimer = window.setInterval(() => {
    const progress = Math.min(1, (download.progress ?? 0) + 0.01);
    if (progress >= 1) {
      window.clearInterval(downloadTimer);
      download = { offered: true, downloading: false, paused: false, progress: null };
      setupState = "ready";
    } else {
      download = { ...download, progress };
    }
    downloadsChanged();
  }, 150);
}

// ---------------------------------------------------------------- AVI files

const ascii = (s: string) => Uint8Array.from(s, (c) => c.charCodeAt(0));

function concat(parts: Uint8Array[]): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

/** Little-endian words: [value, bytes] pairs. */
function words(...fields: [number, 2 | 4][]): Uint8Array {
  const out = new Uint8Array(fields.reduce((n, [, b]) => n + b, 0));
  const view = new DataView(out.buffer);
  let at = 0;
  for (const [v, b] of fields) {
    if (b === 2) view.setUint16(at, v, true);
    else view.setUint32(at, v >>> 0, true);
    at += b;
  }
  return out;
}

const u32s = (...values: number[]) => words(...values.map((v) => [v, 4] as [number, 4]));
const chunk = (id: string, body: Uint8Array) =>
  concat([ascii(id), u32s(body.length), body, new Uint8Array(body.length % 2)]);
const list = (type: string, children: Uint8Array[]) => chunk("LIST", concat([ascii(type), ...children]));

/** An MJPEG AVI as stable-diffusion.cpp writes one: header list, one 00dc chunk per frame, the index. */
function writeAvi(jpegs: Uint8Array[], fps: number, width: number, height: number): Uint8Array<ArrayBuffer> {
  const biggest = Math.max(...jpegs.map((j) => j.length));
  const avih = u32s(Math.round(1_000_000 / fps), biggest * fps, 0, 0x10, jpegs.length, 0, 1, biggest, width, height, 0, 0, 0, 0);
  const strh = concat([
    ascii("vidsMJPG"),
    words([0, 4], [0, 2], [0, 2], [0, 4], [1, 4], [fps, 4], [0, 4], [jpegs.length, 4], [biggest, 4], [0xffffffff, 4], [0, 4]),
    words([0, 2], [0, 2], [width, 2], [height, 2]),
  ]);
  const strf = concat([words([40, 4], [width, 4], [height, 4], [1, 2], [24, 2]), ascii("MJPG"), u32s(width * height * 3, 0, 0, 0, 0)]);
  const hdrl = list("hdrl", [chunk("avih", avih), list("strl", [chunk("strh", strh), chunk("strf", strf)])]);
  const frames = jpegs.map((j) => chunk("00dc", j));
  const movi = list("movi", frames);
  let offset = 4;
  const index = jpegs.map((j, i) => {
    const entry = concat([ascii("00dc"), u32s(0x10, offset, j.length)]);
    offset += frames[i].length;
    return entry;
  });
  const body = concat([ascii("AVI "), hdrl, movi, chunk("idx1", concat(index))]);
  return concat([ascii("RIFF"), u32s(body.length), body]);
}

type Scene = (ctx: CanvasRenderingContext2D, t: number, w: number, h: number) => void;

/** Draws 33 frames at half the clip's size, encodes each as a JPEG and returns the AVI as a blob: URL. */
async function makeAvi(scene: Scene, frames = 33, fps = 16, w = 416, h = 240): Promise<string> {
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("No canvas");
  const jpegs: Uint8Array[] = [];
  for (let i = 0; i < frames; i++) {
    ctx.save();
    scene(ctx, i / frames, w, h);
    ctx.restore();
    const blob = await new Promise<Blob>((resolve, reject) =>
      canvas.toBlob((b) => (b ? resolve(b) : reject(new Error("toBlob failed"))), "image/jpeg", 0.82),
    );
    jpegs.push(new Uint8Array(await blob.arrayBuffer()));
  }
  return URL.createObjectURL(new Blob([writeAvi(jpegs, fps, w, h)], { type: "video/x-msvideo" }));
}

/** A repeatable scatter, so a flake or a drop keeps its place from frame to frame. */
const rand = (i: number) => {
  const x = Math.sin(i * 127.1 + 311.7) * 43758.5453;
  return x - Math.floor(x);
};

const foxScene: Scene = (ctx, t, w, h) => {
  const sky = ctx.createLinearGradient(0, 0, 0, h * 0.65);
  sky.addColorStop(0, "#f2b98f");
  sky.addColorStop(1, "#fde8cf");
  ctx.fillStyle = sky;
  ctx.fillRect(0, 0, w, h);
  ctx.fillStyle = "#fff6e6";
  ctx.beginPath();
  ctx.arc(w * 0.74, h * 0.5, h * 0.11, 0, Math.PI * 2);
  ctx.fill();
  ctx.fillStyle = "#dfe6f0";
  ctx.beginPath();
  ctx.moveTo(0, h * 0.62);
  ctx.quadraticCurveTo(w * 0.3, h * 0.44, w * 0.6, h * 0.58);
  ctx.quadraticCurveTo(w * 0.85, h * 0.5, w, h * 0.6);
  ctx.lineTo(w, h);
  ctx.lineTo(0, h);
  ctx.fill();
  ctx.fillStyle = "#f7f9fc";
  ctx.fillRect(0, h * 0.66, w, h);
  // The fox, bounding left to right.
  const x = -w * 0.1 + t * w * 1.2;
  const y = h * 0.7 - Math.abs(Math.sin(t * Math.PI * 6)) * h * 0.05;
  ctx.fillStyle = "#d2702a";
  ctx.beginPath();
  ctx.ellipse(x, y, 26, 11, -0.08, 0, Math.PI * 2);
  ctx.fill();
  ctx.beginPath();
  ctx.ellipse(x - 34, y - 2, 18, 7, 0.35 + Math.sin(t * Math.PI * 12) * 0.15, 0, Math.PI * 2);
  ctx.fill();
  ctx.beginPath();
  ctx.arc(x + 27, y - 9, 9, 0, Math.PI * 2);
  ctx.fill();
  ctx.beginPath();
  ctx.moveTo(x + 22, y - 15);
  ctx.lineTo(x + 25, y - 26);
  ctx.lineTo(x + 30, y - 16);
  ctx.fill();
  ctx.fillStyle = "#fbf4ec";
  ctx.beginPath();
  ctx.ellipse(x - 49, y - 6, 6, 4, 0.4, 0, Math.PI * 2);
  ctx.fill();
  ctx.fillStyle = "#3a2a20";
  ctx.beginPath();
  ctx.arc(x + 36, y - 8, 2, 0, Math.PI * 2);
  ctx.fill();
  // Falling snow.
  ctx.fillStyle = "rgba(255,255,255,0.9)";
  for (let i = 0; i < 60; i++) {
    const fx = (rand(i) * w + t * 20) % w;
    const fy = (rand(i + 99) * h + t * h * 0.5) % h;
    ctx.beginPath();
    ctx.arc(fx, fy, 1 + rand(i + 7) * 1.5, 0, Math.PI * 2);
    ctx.fill();
  }
};

const forestScene: Scene = (ctx, t, w, h) => {
  ctx.fillStyle = "#4b3a24";
  ctx.fillRect(0, 0, w, h);
  const colours = ["#c8642b", "#e0a33b", "#8f3f22", "#b8862e", "#5f6b2c", "#d9822f"];
  for (let i = 0; i < 90; i++) {
    ctx.fillStyle = colours[i % colours.length];
    ctx.beginPath();
    ctx.arc(rand(i) * w, ((rand(i + 50) * h * 1.2 + t * 18) % (h * 1.2)) - h * 0.1, 12 + rand(i + 3) * 16, 0, Math.PI * 2);
    ctx.fill();
  }
  const fog = ctx.createLinearGradient(0, 0, w, 0);
  const shift = t * 0.6;
  fog.addColorStop(0, "rgba(245,240,230,0)");
  fog.addColorStop(Math.min(0.99, 0.2 + shift * 0.5), "rgba(245,240,230,0.55)");
  fog.addColorStop(1, "rgba(245,240,230,0.1)");
  ctx.fillStyle = fog;
  ctx.fillRect(0, h * 0.3, w, h * 0.45);
};

const boatScene: Scene = (ctx, t, w, h) => {
  const bg = ctx.createLinearGradient(0, 0, 0, h);
  bg.addColorStop(0, "#10152a");
  bg.addColorStop(1, "#1d2742");
  ctx.fillStyle = bg;
  ctx.fillRect(0, 0, w, h);
  for (let i = 0; i < 7; i++) {
    ctx.fillStyle = i % 2 ? "rgba(255,79,163,0.28)" : "rgba(57,208,255,0.25)";
    const bx = (i / 7) * w + Math.sin(t * Math.PI * 2 + i) * 6;
    ctx.fillRect(bx, h * 0.55, 18, h * 0.45);
  }
  const x = w * 0.1 + t * w * 0.7;
  const y = h * 0.66 + Math.sin(t * Math.PI * 4) * 3;
  ctx.fillStyle = "#f1f1ee";
  ctx.beginPath();
  ctx.moveTo(x - 30, y);
  ctx.lineTo(x + 30, y);
  ctx.lineTo(x + 20, y + 12);
  ctx.lineTo(x - 20, y + 12);
  ctx.fill();
  ctx.beginPath();
  ctx.moveTo(x - 4, y);
  ctx.lineTo(x + 4, y - 28);
  ctx.lineTo(x + 18, y);
  ctx.fill();
  ctx.strokeStyle = "rgba(200,215,255,0.5)";
  ctx.lineWidth = 1;
  for (let i = 0; i < 70; i++) {
    const rx = rand(i) * w;
    const ry = (rand(i + 31) * h + t * h * 3) % h;
    ctx.beginPath();
    ctx.moveTo(rx, ry);
    ctx.lineTo(rx - 2, ry + 9);
    ctx.stroke();
  }
};

/** Any other prompt: a slow colour wash with the prompt written on it. */
const promptScene =
  (prompt: string): Scene =>
  (ctx, t, w, h) => {
    const g = ctx.createLinearGradient(0, 0, w, h);
    g.addColorStop(0, `hsl(${(200 + t * 120) % 360} 45% 35%)`);
    g.addColorStop(1, `hsl(${(260 + t * 120) % 360} 45% 22%)`);
    ctx.fillStyle = g;
    ctx.fillRect(0, 0, w, h);
    ctx.fillStyle = "rgba(255,255,255,0.12)";
    ctx.beginPath();
    ctx.arc(w * (0.2 + t * 0.6), h * 0.5, h * 0.35, 0, Math.PI * 2);
    ctx.fill();
    ctx.fillStyle = "#ffffff";
    ctx.font = "500 15px 'DM Sans', sans-serif";
    ctx.textAlign = "center";
    const words = prompt.split(/\s+/);
    const lines: string[] = [];
    let line = "";
    for (const word of words) {
      const next = line ? `${line} ${word}` : word;
      if (ctx.measureText(next).width > w * 0.8 && line) {
        lines.push(line);
        line = word;
      } else line = next;
    }
    if (line) lines.push(line);
    const shown = lines.slice(0, 5);
    shown.forEach((l, i) => ctx.fillText(l, w / 2, h / 2 + (i - (shown.length - 1) / 2) * 20));
  };

function sceneFor(prompt: string): Scene {
  const p = prompt.toLowerCase();
  if (p.includes("fox")) return foxScene;
  if (p.includes("boat")) return boatScene;
  if (p.includes("forest")) return forestScene;
  return promptScene(prompt);
}

// ---------------------------------------------------------------- the first clips

async function seed(): Promise<void> {
  const now = Date.now();
  const MIN = 60_000;
  const HOUR = 60 * MIN;
  const DAY = 24 * HOUR;
  const [fox, forest] = await Promise.all([makeAvi(foxScene), makeAvi(forestScene)]);
  const done = { status: "DONE" as const, width: 832, height: 480, frames: 33, fps: 16 };
  clips = [
    blank({
      id: "vid_mq1",
      prompt: "Waves crash against a lighthouse during a storm, wide shot, dramatic clouds",
      status: "QUEUED",
      createdAt: now - 40_000,
    }),
    blank({
      id: "vid_mr1",
      prompt: "A paper boat drifts down a rainy street at night, neon reflections in the puddles",
      status: "RUNNING",
      stage: "SAMPLING",
      done: 7,
      total: 20,
      createdAt: now - 3 * MIN,
      startedAt: now - 95_000,
    }),
    blank({
      id: "vid_mf1",
      prompt: "A hummingbird hovers at a red flower, macro, shallow depth of field",
      status: "FAILED",
      error:
        "Video engine failed (exit 1). [ERROR] ggml_backend_cuda_buffer_type_alloc_buffer: allocating 2112.00 MiB on device 0: cudaMalloc failed: out of memory",
      createdAt: now - 2 * HOUR,
      startedAt: now - 2 * HOUR + 4000,
    }),
    blank({
      id: "vid_mc1",
      prompt: "Time-lapse of clouds rolling over a mountain ridge at sunset",
      status: "CANCELLED",
      createdAt: now - 3 * HOUR,
      startedAt: now - 3 * HOUR + 2000,
    }),
    blank({
      id: "vid_md1",
      prompt: "A fox runs through fresh snow at dawn, slow motion, soft light",
      ...done,
      file: fox,
      seed: 1_482_907_311,
      elapsedMs: 309_000,
      createdAt: now - DAY,
    }),
    blank({
      id: "vid_md2",
      prompt: "Aerial view of an autumn forest, fog rolling between the trees, golden hour",
      ...done,
      file: forest,
      seed: 88_120_457,
      elapsedMs: 297_000,
      createdAt: now - 2 * DAY - 5 * HOUR,
    }),
  ];
  stageAt = now - 7 * 700;
}

let seeded: Promise<void> | null = null;

function start(): Promise<void> {
  if (!seeded) {
    seeded = seed().then(() => {
      if (flags.has("empty")) clips = [];
      ticker = window.setInterval(tick, 200);
    });
  }
  return seeded;
}

const flags = new Set<string>();

export function registerVideoMocks(): void {
  new URLSearchParams(window.location.search)
    .get("videoMock")
    ?.split(",")
    .forEach((f) => flags.add(f.trim()));
  for (const s of ["no-model", "no-engine", "not-offered"] as const) if (flags.has(s)) setupState = s;
  twoModels = flags.has("two-models");

  mock("video_clips", async () => {
    await start();
    return [...clips].sort((a, b) => b.createdAt - a.createdAt);
  });
  mock("video_submit", async ({ prompt, modelId }) => {
    await start();
    const text = typeof prompt === "string" ? prompt : "";
    if (!text.trim()) throw new Error("Write what the video should show.");
    const p = problem();
    if (p) throw new Error(p);
    const model = twoModels && modelId === OTHER_MODEL.id ? OTHER_MODEL.id : MODEL.id;
    const clip = blank({ id: newId(), prompt: text.trim(), modelId: model, status: "QUEUED", createdAt: Date.now() });
    clips = [...clips, clip];
    changed();
    return clip;
  });
  mock("video_cancel", ({ id }) => {
    const c = clips.find((x) => x.id === id);
    if (!c || c.status === "DONE" || c.status === "FAILED" || c.status === "CANCELLED") return;
    if (c.status === "QUEUED") {
      put({ ...c, status: "CANCELLED" });
      changed();
    } else stopRequested.add(c.id);
  });
  mock("video_delete", ({ id }) => {
    const c = clips.find((x) => x.id === id);
    if (!c || !(c.status === "DONE" || c.status === "FAILED" || c.status === "CANCELLED")) return false;
    clips = clips.filter((x) => x.id !== id);
    if (c.file?.startsWith("blob:")) URL.revokeObjectURL(c.file);
    changed();
    return true;
  });
  mock("video_setup", ({ modelId }): VideoSetup => {
    const chosen = twoModels && modelId === OTHER_MODEL.id ? OTHER_MODEL : MODEL;
    const modelIn = setupState === "ready" || setupState === "no-engine";
    const engineIn = setupState === "ready";
    const big = chosen === OTHER_MODEL;
    return {
      problem: problem(),
      modelId: chosen.id,
      modelName: chosen.name,
      downloadBytes: (modelIn ? 0 : MODEL_BYTES) + (engineIn ? 0 : ENGINE_BYTES),
      width: big ? 1280 : 832,
      height: big ? 704 : 480,
      frames: big ? 121 : 33,
      fps: big ? 24 : 16,
      models: modelIn ? (twoModels ? [MODEL, OTHER_MODEL] : [MODEL]) : [],
      folder: FOLDER,
    };
  });
  mock("video_download_state", () => ({ ...download, offered: setupState !== "not-offered" }));
  mock("video_download", () => runDownload());
  mock("video_download_pause", () => {
    window.clearInterval(downloadTimer);
    download = { ...download, downloading: false, paused: true };
    downloadsChanged();
  });
  mock("video_download_resume", () => runDownload());
  mock("video_open_folder", () => console.info(`[mock] open ${FOLDER}`));
  mock("video_open", ({ id }) => console.info(`[mock] open clip ${String(id)}`));
  mock("video_reveal", ({ id }) => console.info(`[mock] show clip ${String(id)} in Explorer`));

  (window as unknown as { nookVideoMock: unknown }).nookVideoMock = {
    /** "ready", "no-model", "no-engine" or "not-offered". */
    setup(state: SetupState) {
      setupState = state;
      window.clearInterval(downloadTimer);
      download = { offered: true, downloading: false, paused: false, progress: null };
      downloadsChanged();
    },
    twoModels(on = true) {
      twoModels = on;
      downloadsChanged();
    },
    clear() {
      clips = [];
      changed();
    },
    /** Adds a finished clip whose file is not an AVI, for the player's "can't play here". */
    broken() {
      const file = URL.createObjectURL(new Blob(["not a video"], { type: "video/x-msvideo" }));
      clips = [
        ...clips,
        blank({ id: newId(), prompt: "A clip the page cannot read", status: "DONE", file, width: 832, height: 480, frames: 33, fps: 16, createdAt: Date.now() }),
      ];
      changed();
    },
    stop() {
      window.clearInterval(ticker);
    },
  };
}
