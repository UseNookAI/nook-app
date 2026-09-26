/**
 * Browser stand-ins for the flows commands (FlowService, its plan, its one download, the
 * microphone). A run moves through the stages on a timer, one at a time, as the service's single
 * worker does; a finished run's track is a real WAV made here (a voice-like hum, one burst per
 * line), so the page's own player plays it.
 *
 * States to look at: `?flowsMock=fresh` (nothing downloaded yet), `no-model` (no chat model),
 * `empty` (no runs), comma-separated; or `nookFlowsMock.fresh()`, `nookFlowsMock.clear()`,
 * `nookFlowsMock.failNext()` in the console.
 */
import type { Install, Language, Need, Plan, Run, Segment, Stage } from "../flows";
import { mock, mockEmit } from "../ipc";

/** Heard by Whisper but spoken by none of the voices. */
const UNSPOKEN = ["is", "sr", "fa", "ur", "bn", "ta", "te", "ml", "ca"];
const LANGUAGES: Language[] = [
  ["en", "English"], ["es", "Spanish"], ["fr", "French"], ["de", "German"], ["it", "Italian"], ["pt", "Portuguese"],
  ["nl", "Dutch"], ["sv", "Swedish"], ["da", "Danish"], ["no", "Norwegian"], ["fi", "Finnish"], ["is", "Icelandic"],
  ["pl", "Polish"], ["cs", "Czech"], ["sk", "Slovak"], ["hu", "Hungarian"], ["ro", "Romanian"], ["bg", "Bulgarian"],
  ["hr", "Croatian"], ["sr", "Serbian"], ["sl", "Slovenian"], ["uk", "Ukrainian"], ["ru", "Russian"], ["el", "Greek"],
  ["tr", "Turkish"], ["ar", "Arabic"], ["he", "Hebrew"], ["fa", "Persian"], ["ur", "Urdu"], ["hi", "Hindi"],
  ["bn", "Bengali"], ["ta", "Tamil"], ["te", "Telugu"], ["ml", "Malayalam"], ["id", "Indonesian"], ["ms", "Malay"],
  ["vi", "Vietnamese"], ["th", "Thai"], ["zh", "Chinese"], ["ja", "Japanese"], ["ko", "Korean"], ["ca", "Catalan"],
  ["et", "Estonian"], ["lv", "Latvian"], ["lt", "Lithuanian"], ["sw", "Swahili"], ["tl", "Tagalog"],
].map(([code, name]) => ({ code, name, spoken: !UNSPOKEN.includes(code) }));

/** runtime/voices.json, as the plan reads it. */
const VOICES = [
  { id: "qwen3", name: "Qwen3-TTS", clones: true, bytes: 1_991_211_136, languages: "en zh ja ko de fr ru pt es it" },
  {
    id: "voxcpm2",
    name: "VoxCPM2",
    clones: true,
    designs: true,
    bytes: 2_955_000_480,
    languages: "ar da de el en es fi fr he hi id it ja km ko lo ms my nl no pl pt ru sv sw th tl tr vi zh",
  },
  {
    id: "supertonic",
    name: "Supertonic",
    clones: false,
    bytes: 312_784_196,
    languages: "en ko ja ar bg cs da de el es et fi fr hi hr hu id it lt lv nl pl pt ro ru sk sl sv tr uk vi",
  },
].map((v) => ({ designs: false, ...v, languages: v.languages.split(" ") }));

const SPEECH_BYTES = 574_041_195;
const WHISPER_BYTES = 382_000_000;
const ENGINE_BYTES = 1_067_893_753;
const FFMPEG_BYTES = 80_726_424;
const FOLDER = "C:\\Users\\you\\AppData\\Local\\Nook-rs\\flows";
const MODEL = { id: "qwen3-8b-q4km", name: "Qwen3 8B" };

const params = new URLSearchParams(window.location.search).get("flowsMock")?.split(",") ?? [];
const installed = {
  speech: !params.includes("fresh"),
  engine: !params.includes("fresh"),
  ffmpeg: false,
  voices: new Set(params.includes("fresh") ? [] : ["qwen3", "supertonic"]),
  model: !params.includes("no-model"),
};
let install: Install | null = null;
let installTimer: number | undefined;
let runs: Run[] = [];
let failNext = false;
const stops = new Set<string>();
let stageAt = 0;
let ticker: number | undefined;
let levelTimer: number | undefined;

const name = (code: string) => LANGUAGES.find((l) => l.code === code)?.name ?? code;
const emitRun = (id: string) => {
  const run = runs.find((r) => r.id === id);
  if (run) mockEmit("flows", { run });
};

// ---------------------------------------------------------------- what is said

const SAID = [
  "Thanks for joining us today.",
  "We are going to talk about running AI on your own computer.",
  "Everything stays on this machine, nothing goes to the cloud.",
  "The first step is listening to what was said.",
  "Then a local model translates it, line by line.",
  "And finally a voice speaks it again, in your own voice.",
  "It works for recordings, podcasts and videos.",
  "Let's see how well it does.",
];

const TRANSLATED: Record<string, string[]> = {
  es: [
    "Gracias por acompañarnos hoy.",
    "Vamos a hablar de ejecutar IA en tu propio ordenador.",
    "Todo se queda en esta máquina, nada va a la nube.",
    "El primer paso es escuchar lo que se dijo.",
    "Después un modelo local lo traduce, línea por línea.",
    "Y por último una voz lo dice de nuevo, con tu propia voz.",
    "Funciona con grabaciones, pódcasts y vídeos.",
    "Veamos qué tal lo hace.",
  ],
  de: [
    "Danke, dass Sie heute dabei sind.",
    "Wir sprechen darüber, KI auf dem eigenen Computer laufen zu lassen.",
    "Alles bleibt auf diesem Rechner, nichts geht in die Cloud.",
    "Der erste Schritt ist, zuzuhören, was gesagt wurde.",
    "Dann übersetzt ein lokales Modell es, Zeile für Zeile.",
    "Und zuletzt spricht eine Stimme es noch einmal, mit Ihrer eigenen Stimme.",
    "Es funktioniert mit Aufnahmen, Podcasts und Videos.",
    "Mal sehen, wie gut es das macht.",
  ],
  fr: [
    "Merci d'être avec nous aujourd'hui.",
    "Nous allons parler de l'IA qui tourne sur votre propre ordinateur.",
    "Tout reste sur cette machine, rien ne part dans le cloud.",
    "La première étape consiste à écouter ce qui a été dit.",
    "Ensuite, un modèle local le traduit, ligne par ligne.",
    "Et enfin une voix le redit, avec votre propre voix.",
    "Cela marche pour les enregistrements, les podcasts et les vidéos.",
    "Voyons ce que ça donne.",
  ],
};

function segments(count: number, target: string, translated: boolean): Segment[] {
  const out: Segment[] = [];
  let t = 0.6;
  for (let i = 0; i < count; i++) {
    const text = SAID[i % SAID.length];
    const len = 1.6 + text.length / 22;
    out.push({ start: t, end: t + len, text, translation: translated ? (TRANSLATED[target] ?? SAID)[i % SAID.length] : null });
    t += len + 0.35;
  }
  return out;
}

/** A voice-like hum as a 16-bit WAV: one burst of syllables per line, laid out as the lines are. */
function track(lines: Segment[], compact: boolean): string {
  const rate = 22_050;
  const spans = compact
    ? lines.reduce<{ start: number; len: number }[]>((acc, s) => {
        const start = acc.length ? acc[acc.length - 1].start + acc[acc.length - 1].len + 0.3 : 0.3;
        return [...acc, { start, len: (s.end - s.start) * 0.9 }];
      }, [])
    : lines.map((s) => ({ start: s.start, len: (s.end - s.start) * 0.9 }));
  const seconds = (spans.length ? spans[spans.length - 1].start + spans[spans.length - 1].len : 1) + 0.4;
  const n = Math.floor(seconds * rate);
  const data = new Int16Array(n);
  spans.forEach((span, k) => {
    const pitch = 150 + (k % 3) * 18;
    const from = Math.floor(span.start * rate);
    const to = Math.min(n, Math.floor((span.start + span.len) * rate));
    for (let i = from; i < to; i++) {
      const t = (i - from) / rate;
      const syllable = Math.max(0, Math.sin(t * Math.PI * 4.2)) ** 0.6;
      const edge = Math.min(1, t / 0.05, (span.len - t) / 0.08);
      const f = pitch * (1 + 0.04 * Math.sin(t * 3));
      const v =
        Math.sin(2 * Math.PI * f * t) * 0.5 +
        Math.sin(2 * Math.PI * 2 * f * t) * 0.25 +
        Math.sin(2 * Math.PI * 3 * f * t) * 0.12;
      data[i] = Math.round(v * syllable * edge * 0.35 * 32767);
    }
  });
  const header = new DataView(new ArrayBuffer(44));
  const text = (at: number, s: string) => [...s].forEach((c, i) => header.setUint8(at + i, c.charCodeAt(0)));
  text(0, "RIFF");
  header.setUint32(4, 36 + n * 2, true);
  text(8, "WAVE");
  text(12, "fmt ");
  header.setUint32(16, 16, true);
  header.setUint16(20, 1, true);
  header.setUint16(22, 1, true);
  header.setUint32(24, rate, true);
  header.setUint32(28, rate * 2, true);
  header.setUint16(32, 2, true);
  header.setUint16(34, 16, true);
  text(36, "data");
  header.setUint32(40, n * 2, true);
  return URL.createObjectURL(new Blob([header.buffer, data.buffer], { type: "audio/wav" }));
}

// ---------------------------------------------------------------- the plan

function pick(language: string, keepVoice: boolean) {
  const speaks = (v: (typeof VOICES)[number]) => v.languages.includes(language);
  const cloning = VOICES.find((v) => v.clones && speaks(v));
  const preset = VOICES.find((v) => !v.clones && speaks(v));
  const designing = VOICES.find((v) => v.designs && speaks(v));
  if (keepVoice) {
    if (cloning) return { voice: cloning, cloned: true, note: null };
    if (preset)
      return { voice: preset, cloned: false, note: `No voice can clone a speaker in ${name(language)} yet, so it is spoken in a standard voice.` };
    return null;
  }
  if (preset) return { voice: preset, cloned: false, note: null };
  if (designing) return { voice: designing, cloned: false, note: null };
  if (cloning) return { voice: cloning, cloned: true, note: `The only voice for ${name(language)} clones the speaker, so it is spoken in their voice.` };
  return null;
}

const extension = (path: string) => path.split(".").pop()?.toLowerCase() ?? "";
const isVideo = (path: string) => ["mp4", "m4v", "mov", "mkv", "webm", "avi", "mpg", "mpeg", "wmv", "3gp", "ts"].includes(extension(path));
const readable = (path: string) => !["opus", "wma", "amr"].includes(extension(path));

function plan(input: string | null, microphone: boolean, target: string, keepVoice: boolean): Plan {
  let problem: string | null = !microphone && !input ? "Choose an audio or video file." : null;
  if (!problem && !installed.model) problem = "No model to translate with is installed. Download one in Settings > Models.";
  const choice = pick(target, keepVoice);
  const needs: Need[] = [];
  if (!installed.speech) needs.push({ what: "the speech model", bytes: SPEECH_BYTES + WHISPER_BYTES });
  if (!microphone && input && !installed.ffmpeg) {
    if (isVideo(input)) needs.push({ what: "FFmpeg, to put the video back together", bytes: FFMPEG_BYTES });
    else if (!readable(input)) needs.push({ what: `FFmpeg, to read .${extension(input)} files`, bytes: FFMPEG_BYTES });
  }
  if (choice) {
    if (!installed.engine) needs.push({ what: "the voice engine", bytes: ENGINE_BYTES });
    if (!installed.voices.has(choice.voice.id)) needs.push({ what: `the ${choice.voice.name} voice`, bytes: choice.voice.bytes });
  }
  const whose = microphone ? "your own voice" : "the speaker's own voice";
  let spokenWith = !choice
    ? `Nook has no voice for ${name(target)} yet, so this run gives the text and subtitles.`
    : choice.cloned
      ? `${name(target)} will be spoken by ${choice.voice.name} in ${whose}.`
      : `${name(target)} will be spoken in a standard voice by ${choice.voice.name}.`;
  if (choice?.note) spokenWith = `${choice.note} ${spokenWith}`;
  const totalBytes = needs.reduce((a, n) => a + n.bytes, 0);
  return {
    voiceName: choice?.voice.name ?? null,
    cloned: choice?.cloned ?? false,
    spokenWith,
    noVoice: choice == null,
    needs,
    totalBytes,
    problem,
    ready: problem == null && needs.length === 0,
    modelId: installed.model ? MODEL.id : null,
    modelName: installed.model ? MODEL.name : null,
  };
}

// ---------------------------------------------------------------- runs

function newId(): string {
  return "flow_" + Date.now().toString(36) + Math.random().toString(16).slice(2, 8);
}

function blank(over: Partial<Run> & Pick<Run, "input" | "inputName" | "source" | "targetLanguage">): Run {
  return {
    id: newId(),
    flow: "translate-audio",
    sourceLanguage: null,
    modelId: MODEL.id,
    keepVoice: true,
    voiceName: null,
    cloned: false,
    note: null,
    status: "QUEUED",
    stage: null,
    done: 0,
    total: 0,
    detectedLanguage: null,
    durationSeconds: 0,
    segments: [],
    audio: null,
    video: null,
    elapsedMs: 0,
    error: null,
    createdAt: Date.now(),
    startedAt: null,
    ...over,
  };
}

const put = (r: Run) => {
  runs = runs.map((x) => (x.id === r.id ? r : x));
  emitRun(r.id);
};

function move(r: Run, stage: Stage, done: number, total: number) {
  stageAt = Date.now();
  put({ ...r, stage, done, total });
}

function finish(r: Run) {
  const choice = pick(r.targetLanguage, r.keepVoice);
  const lines = r.segments.map((s, i) => ({ ...s, translation: (TRANSLATED[r.targetLanguage] ?? SAID)[i % SAID.length] }));
  put({
    ...r,
    status: "DONE",
    stage: null,
    done: 0,
    total: 0,
    segments: lines,
    voiceName: choice?.voice.name ?? null,
    cloned: choice?.cloned ?? false,
    note: choice ? choice.note : `Nook has no voice for ${name(r.targetLanguage)} yet, so this run gives the text and subtitles.`,
    audio: choice ? track(lines, r.source === "MICROPHONE") : null,
    video: isVideo(r.input) ? r.input.replace(/\.[^.]+$/, `.${r.targetLanguage}.mp4`) : null,
    elapsedMs: Date.now() - (r.startedAt ?? Date.now()),
  });
}

function tick() {
  const now = Date.now();
  const r = runs.find((x) => x.status === "RUNNING");
  if (!r) {
    const next = [...runs].filter((x) => x.status === "QUEUED").sort((a, b) => a.createdAt - b.createdAt)[0];
    if (!next) return;
    stageAt = now;
    put({ ...next, status: "RUNNING", stage: "PREPARING", startedAt: now });
    return;
  }
  if (stops.has(r.id)) {
    stops.delete(r.id);
    put({ ...r, status: "CANCELLED", stage: null, done: 0, total: 0 });
    return;
  }
  const inStage = now - stageAt;
  const count = r.source === "MICROPHONE" ? 2 : 8;
  switch (r.stage) {
    case "PREPARING":
      if (inStage > 500) move(r, "LISTENING", 0, r.source === "MICROPHONE" ? 1 : 2);
      break;
    case "LISTENING":
      if (inStage > 900 * r.total) {
        const heard = segments(count, r.targetLanguage, false);
        put({
          ...r,
          detectedLanguage: r.sourceLanguage ?? "en",
          durationSeconds: heard[heard.length - 1].end + 0.5,
          segments: heard,
        });
        const now2 = runs.find((x) => x.id === r.id)!;
        if (failNext) {
          failNext = false;
          put({ ...now2, status: "FAILED", stage: null, error: "The voice engine stopped (exit 1). model failed to load" });
          return;
        }
        move(now2, "TRANSLATING", 0, count);
      } else if (Math.floor(inStage / 900) !== r.done) put({ ...r, done: Math.min(r.total - 1, Math.floor(inStage / 900)) });
      break;
    case "TRANSLATING": {
      const d = Math.min(r.total, Math.floor(inStage / 250));
      if (d >= r.total) {
        const lines = r.segments.map((s, i) => ({ ...s, translation: (TRANSLATED[r.targetLanguage] ?? SAID)[i % SAID.length] }));
        const withLines = { ...r, segments: lines };
        runs = runs.map((x) => (x.id === r.id ? withLines : x));
        if (!pick(r.targetLanguage, r.keepVoice)) move(withLines, "SAVING", 0, 0);
        else move(withLines, "SPEAKING", 0, r.total);
      } else if (d !== r.done) put({ ...r, done: d });
      break;
    }
    case "SPEAKING": {
      const d = Math.min(r.total, Math.floor(inStage / 400));
      if (d >= r.total) move(r, "ASSEMBLING", 0, 0);
      else if (d !== r.done) put({ ...r, done: d });
      break;
    }
    case "ASSEMBLING":
      if (inStage > 500) move(r, "SAVING", 0, 0);
      break;
    case "SAVING":
      if (inStage > 300) finish(r);
      break;
  }
}

function queue(run: Run): Run {
  runs = [run, ...runs];
  emitRun(run.id);
  if (ticker === undefined) ticker = window.setInterval(tick, 120);
  return run;
}

function check(input: string | null, microphone: boolean, target: string, keepVoice: boolean) {
  const p = plan(input, microphone, target, keepVoice);
  if (p.problem) throw new Error(p.problem);
  if (p.needs.length) throw new Error(`This run needs a download first: ${p.needs[0].what}.`);
}

function seed() {
  if (params.includes("empty")) return;
  const hour = 3_600_000;
  const talk = segments(8, "es", true);
  const said = segments(2, "de", true);
  runs = [
    blank({
      input: `${FOLDER}\\flow_seed1\\recording.wav`,
      inputName: "Recording",
      source: "MICROPHONE",
      targetLanguage: "de",
      status: "DONE",
      detectedLanguage: "en",
      durationSeconds: 7.4,
      segments: said,
      voiceName: "Qwen3-TTS",
      cloned: true,
      audio: track(said, true),
      elapsedMs: 9_200,
      createdAt: Date.now() - 0.2 * hour,
    }),
    blank({
      input: "C:\\Users\\you\\Podcasts\\local-ai-episode-12.mp3",
      inputName: "local-ai-episode-12.mp3",
      source: "FILE",
      targetLanguage: "es",
      status: "DONE",
      detectedLanguage: "en",
      durationSeconds: talk[talk.length - 1].end + 0.5,
      segments: talk,
      voiceName: "Qwen3-TTS",
      cloned: true,
      audio: track(talk, false),
      elapsedMs: 41_000,
      createdAt: Date.now() - 26 * hour,
    }),
  ];
}

// ---------------------------------------------------------------- the microphone

function stopLevels() {
  window.clearInterval(levelTimer);
  levelTimer = undefined;
}

export function registerFlowsMocks(): void {
  seed();

  mock("flows_languages", () => LANGUAGES);
  mock("flows_runs", () => runs);
  mock("flows_plan", (a) => plan(a.input as string | null, a.microphone as boolean, a.target as string, a.keepVoice as boolean));
  mock("flows_install_state", () => install);
  mock("flows_install", (a) => {
    if (install && !install.error) return;
    const p = plan(a.input as string | null, a.microphone as boolean, a.target as string, a.keepVoice as boolean);
    if (!p.needs.length) return;
    let done = 0;
    const step = p.totalBytes / 40;
    install = { what: p.needs[0].what, done: 0, total: p.totalBytes, error: null };
    mockEmit("flows", { install });
    installTimer = window.setInterval(() => {
      done = Math.min(p.totalBytes, done + step);
      let before = 0;
      const now = p.needs.find((n) => (before += n.bytes) > done) ?? p.needs[p.needs.length - 1];
      install = { what: now.what, done, total: p.totalBytes, error: null };
      if (done >= p.totalBytes) {
        window.clearInterval(installTimer);
        installed.speech = true;
        installed.engine = true;
        installed.ffmpeg = installed.ffmpeg || p.needs.some((n) => n.what.startsWith("FFmpeg"));
        const choice = pick(a.target as string, a.keepVoice as boolean);
        if (choice) installed.voices.add(choice.voice.id);
        install = null;
      }
      mockEmit("flows", { install });
    }, 150);
  });
  mock("flows_cancel_install", () => {
    window.clearInterval(installTimer);
    if (install) install = { ...install, error: "The download was stopped." };
    mockEmit("flows", { install });
  });
  mock("flows_clear_install_error", () => {
    if (install?.error) install = null;
    mockEmit("flows", { install });
  });

  mock("flows_submit", (a) => {
    const input = a.input as string;
    check(input, false, a.target as string, a.keepVoice as boolean);
    return queue(
      blank({
        input,
        inputName: input.split(/[\\/]/).pop() ?? input,
        source: "FILE",
        sourceLanguage: (a.source as string | null) ?? null,
        targetLanguage: a.target as string,
        keepVoice: a.keepVoice as boolean,
      }),
    );
  });

  mock("flows_record_start", () => {
    stopLevels();
    const started = performance.now();
    levelTimer = window.setInterval(() => {
      const t = (performance.now() - started) / 1000;
      const swell = 0.5 + 0.5 * Math.sin(t * 1.3);
      const syllable = Math.abs(Math.sin(t * 9.0));
      const gap = Math.sin(t * 0.7) > 0.85 ? 0.1 : 1;
      mockEmit("speech", { kind: "level", level: Math.min(1, 0.02 + 0.4 * swell * syllable * gap + Math.random() * 0.05) });
    }, 45);
  });
  mock("flows_record_stop", async (a) => {
    stopLevels();
    await new Promise((r) => setTimeout(r, 300));
    check(null, true, a.target as string, a.keepVoice as boolean);
    const id = newId();
    return queue(
      blank({
        id,
        input: `${FOLDER}\\${id}\\recording.wav`,
        inputName: "Recording",
        source: "MICROPHONE",
        sourceLanguage: (a.source as string | null) ?? null,
        targetLanguage: a.target as string,
        keepVoice: a.keepVoice as boolean,
      }),
    );
  });
  mock("flows_record_cancel", () => stopLevels());

  mock("flows_cancel", (a) => {
    const r = runs.find((x) => x.id === a.id);
    if (!r) return;
    if (r.status === "QUEUED") put({ ...r, status: "CANCELLED" });
    else if (r.status === "RUNNING") stops.add(r.id);
  });
  mock("flows_delete", (a) => {
    const r = runs.find((x) => x.id === a.id);
    if (!r || !["DONE", "FAILED", "CANCELLED"].includes(r.status)) return false;
    runs = runs.filter((x) => x.id !== a.id);
    mockEmit("flows", { removed: a.id });
    return true;
  });
  mock("flows_again", (a) => {
    const old = runs.find((x) => x.id === a.id);
    if (!old) throw new Error("That run is gone.");
    return queue(
      blank({
        input: old.input,
        inputName: old.inputName,
        source: old.source,
        sourceLanguage: old.sourceLanguage,
        targetLanguage: old.targetLanguage,
        keepVoice: old.keepVoice,
      }),
    );
  });
  mock("flows_open_folder", () => undefined);
  mock("flows_open", () => undefined);
  mock("flows_reveal", () => undefined);

  (window as unknown as Record<string, unknown>).nookFlowsMock = {
    fresh: () => {
      installed.speech = false;
      installed.engine = false;
      installed.voices.clear();
      mockEmit("downloads", {});
    },
    clear: () => {
      runs.forEach((r) => mockEmit("flows", { removed: r.id }));
      runs = [];
    },
    failNext: () => {
      failNext = true;
    },
  };
}
