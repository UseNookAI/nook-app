import { describe, expect, it } from "vitest";
import { clipProgress, type Clip } from "../../api/video";
import { clipText, clock, doneText, gigabytes, seconds, stageText } from "./format";

const clip = (over: Partial<Clip>): Clip => ({
  id: "vid_1",
  prompt: "p",
  modelId: "wan2.1-t2v-1.3b",
  status: "RUNNING",
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
  createdAt: new Date(2026, 8, 24, 9, 5).getTime(),
  startedAt: null,
  ...over,
});

describe("video wording", () => {
  it("writes seconds as the original does", () => {
    expect(seconds(33 / 16)).toBe("2.1 s");
    expect(seconds(3)).toBe("3 s");
    expect(seconds(12.6)).toBe("13 s");
    expect(seconds(0)).toBe("0 s");
  });

  it("writes a clock", () => {
    expect(clock(0)).toBe("0:00");
    expect(clock(65_900)).toBe("1:05");
    expect(clock(3_723_000)).toBe("1:02:03");
    expect(clock(-5)).toBe("0:00");
  });

  it("writes gigabytes", () => {
    expect(gigabytes(5_935_462_998)).toBe("5.9 GB");
    expect(gigabytes(0)).toBe("0.0 GB");
  });

  it("names the stages", () => {
    expect(stageText(clip({}))).toBe("Starting");
    expect(stageText(clip({ stage: "LOADING" }))).toBe("Reading the prompt");
    expect(stageText(clip({ stage: "SAMPLING" }))).toBe("Rendering");
    expect(stageText(clip({ stage: "SAMPLING", done: 7, total: 20 }))).toBe("Rendering · step 7 of 20");
    expect(stageText(clip({ stage: "DECODING", done: 1, total: 8 }))).toBe("Decoding the frames");
    expect(stageText(clip({ stage: "SAVING" }))).toBe("Saving");
  });

  it("describes a finished clip and the setup", () => {
    const done = clip({ status: "DONE", width: 832, height: 480, frames: 33, fps: 16, seed: 42, elapsedMs: 309_000 });
    expect(doneText(done)).toBe("2.1 s · 832×480 · made in 5:09 · seed 42 · 24 Sep, 09:05");
    expect(
      clipText({ problem: null, modelId: null, modelName: "", downloadBytes: 0, width: 832, height: 480, frames: 33, fps: 16, models: [], folder: "" }),
    ).toBe("832×480 · 2.1 s clips");
  });

  it("shares the progress bar out by stage", () => {
    expect(clipProgress(clip({ status: "QUEUED" }))).toBe(0);
    expect(clipProgress(clip({ stage: "LOADING" }))).toBe(0);
    expect(clipProgress(clip({ stage: "SAMPLING", done: 10, total: 20 }))).toBeCloseTo(0.415);
    expect(clipProgress(clip({ stage: "DECODING", done: 8, total: 8 }))).toBeCloseTo(0.99);
    expect(clipProgress(clip({ stage: "SAVING" }))).toBe(0.99);
    expect(clipProgress(clip({ status: "DONE" }))).toBe(1);
  });
});
