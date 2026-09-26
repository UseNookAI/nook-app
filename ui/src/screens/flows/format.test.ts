import { describe, expect, it } from "vitest";
import { runProgress, type Plan, type Run } from "../../api/flows";
import { bytesText, defaultTarget, doneText, languageName, needsText, splitPath, stageText, translationText, voiceText } from "./format";

const languages = [
  { code: "en", name: "English" },
  { code: "es", name: "Spanish" },
  { code: "de", name: "German" },
];

function run(over: Partial<Run> = {}): Run {
  return {
    id: "flow_1",
    flow: "translate-audio",
    input: "C:\\talks\\talk.mp3",
    inputName: "talk.mp3",
    source: "FILE",
    sourceLanguage: null,
    targetLanguage: "de",
    modelId: "qwen3-8b",
    keepVoice: true,
    voiceName: null,
    cloned: false,
    note: null,
    status: "DONE",
    stage: null,
    done: 0,
    total: 0,
    detectedLanguage: "en",
    durationSeconds: 25,
    segments: [
      { start: 0, end: 2, text: "Hello.", translation: "Hallo." },
      { start: 2, end: 4, text: "Bye.", translation: " Tschüss. " },
    ],
    audio: null,
    video: null,
    elapsedMs: 63_000,
    error: null,
    createdAt: 0,
    startedAt: null,
    ...over,
  };
}

describe("flows wording", () => {
  it("sizes read as megabytes and gigabytes", () => {
    expect(bytesText(80_726_424)).toBe("81 MB");
    expect(bytesText(1_991_211_136)).toBe("2.0 GB");
    expect(bytesText(10)).toBe("1 MB");
  });

  it("names languages and picks a first target", () => {
    expect(languageName("de", languages)).toBe("German");
    expect(languageName("welsh", languages)).toBe("Welsh");
    expect(languageName(null, languages)).toBe("");
    expect(defaultTarget("en-US", languages)).toBe("es");
    expect(defaultTarget("de-AT", languages)).toBe("de");
    expect(defaultTarget("xx", languages)).toBe("es");
  });

  it("says what a run is doing and what it made", () => {
    expect(stageText(run({ status: "RUNNING", stage: "LISTENING", done: 1, total: 3 }))).toBe("Listening · part 2 of 3");
    expect(stageText(run({ status: "RUNNING", stage: "LISTENING", done: 0, total: 1 }))).toBe("Listening");
    expect(stageText(run({ status: "RUNNING", stage: "SPEAKING", done: 4, total: 7 }))).toBe("Speaking · 4 of 7 lines");
    expect(stageText(run({ status: "RUNNING", stage: "PREPARING", source: "MICROPHONE" }))).toBe("Getting the recording ready");
    expect(doneText(run())).toBe("0:25 of speech · 2 lines · done in 1:03 · qwen3-8b");
    expect(voiceText(run())).toBeNull();
    expect(voiceText(run({ voiceName: "Qwen3-TTS", cloned: true, source: "MICROPHONE" }))).toBe("Qwen3-TTS, in your own voice");
    expect(voiceText(run({ voiceName: "Supertonic" }))).toBe("Supertonic, a standard voice");
    expect(translationText(run())).toBe("Hallo.\nTschüss.");
  });

  it("puts every download on one line", () => {
    const plan: Plan = {
      voiceName: "Qwen3-TTS",
      cloned: true,
      spokenWith: "",
      noVoice: false,
      needs: [
        { what: "the voice engine", bytes: 59_719_330 },
        { what: "the Qwen3-TTS voice", bytes: 1_991_211_136 },
      ],
      totalBytes: 2_050_930_466,
      problem: null,
      ready: false,
      modelId: null,
      modelName: null,
    };
    expect(needsText(plan)).toBe(
      "One-time download: the voice engine (60 MB), the Qwen3-TTS voice (2.0 GB), 2.1 GB in all. It stays on this computer, and everything runs here.",
    );
  });

  it("measures progress by stage", () => {
    expect(runProgress(run())).toBe(1);
    expect(runProgress(run({ status: "RUNNING", stage: "SPEAKING", done: 1, total: 2 }))).toBeCloseTo(0.75);
    expect(runProgress(run({ status: "QUEUED" }))).toBe(0);
    expect(splitPath("C:\\a\\b.mp3")).toEqual({ name: "b.mp3", folder: "C:\\a" });
  });
});
