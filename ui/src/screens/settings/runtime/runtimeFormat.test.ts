/** RuntimeSettingsView.kt wording. */
import { describe, expect, it } from "vitest";
import type { EngineInfo, GpuDevice, SpeedProbeResult } from "../../../api/runtime";
import {
  downloadPercent,
  engineLayout,
  engineQueue,
  eventFailed,
  eventLine,
  gpuDriverText,
  gpuUsedShare,
  gpuUsedText,
  javaDouble,
  probeDescription,
  probeTitle,
  stateText,
} from "./runtimeFormat";

const MIB = 1024 * 1024;
const gpu: GpuDevice = {
  index: 0,
  name: "NVIDIA GeForce RTX 4060",
  totalBytes: 8188 * MIB,
  freeBytes: 1100 * MIB,
  driverVersion: "581.29",
  computeCapability: "8.9",
  vendor: "nvidia",
  integrated: false,
};
const engine = (over: Partial<EngineInfo> = {}): EngineInfo => ({
  modelId: "gpt-oss-20b",
  displayName: "gpt-oss 20B",
  state: "READY",
  port: 41500,
  gpuLayers: 24,
  layers: 24,
  ctxPerSlot: 8192,
  slots: 1,
  inFlight: 1,
  pinned: false,
  startedAt: null,
  lastUsed: null,
  failure: null,
  active: 1,
  waitingInteractive: 0,
  waitingBackground: 2,
  tensorSplit: null,
  ...over,
});
const probe = (generateTps: number): SpeedProbeResult => ({
  modelId: "gpt-oss-20b",
  sha256: null,
  driver: "581.29",
  backend: "cuda",
  gpuLayers: 24,
  promptTps: 412.7,
  generateTps,
  measuredAt: "2026-09-25T10:00:00Z",
});

describe("runtime wording", () => {
  it("prints doubles as Kotlin did", () => {
    expect(javaDouble(31)).toBe("31.0");
    expect(javaDouble(31.4)).toBe("31.4");
  });

  it("says how full each card is", () => {
    expect(gpuUsedText(gpu)).toBe("6.9 of 8 GB used");
    expect(gpuUsedShare(gpu).nearlyFull).toBe(false);
    expect(gpuUsedShare({ ...gpu, freeBytes: 300 * MIB }).nearlyFull).toBe(true);
    expect(gpuUsedShare({ ...gpu, totalBytes: 0, freeBytes: 0 }).share).toBe(0);
    expect(gpuDriverText(gpu, 588 * MIB)).toBe("Driver 581.29 · 0.6 GB available for models");
    expect(gpuDriverText({ ...gpu, driverVersion: null }, 0)).toBe("Driver unknown · 0.0 GB available for models");
  });

  it("titles a probe with the loaded model's name and says whether it is fast enough", () => {
    expect(probeTitle(probe(31.4), [engine()])).toBe("gpt-oss 20B · 31.4 tokens per second");
    expect(probeTitle(probe(31), [])).toBe("gpt-oss-20b · 31.0 tokens per second");
    const now = Date.parse("2026-09-25T12:30:00Z");
    expect(probeDescription(probe(31.4), now)).toBe("Reads 412 tokens per second · fast enough for Nook Code · measured 2 h ago with driver 581.29");
    expect(probeDescription(probe(6.3), now)).toContain("too slow for Nook Code (under 8 tokens per second)");
  });

  it("describes a loaded engine's layers, context and queue", () => {
    expect(stateText("READY")).toBe("Ready");
    expect(engineLayout(engine())).toBe("24/24 layers on GPU · context 8192 per request · 1 slot");
    expect(engineLayout(engine({ gpuLayers: 99, layers: 36, slots: 2, tensorSplit: "3,1" }))).toBe(
      "36/36 layers on GPU · context 8192 per request · 2 slots · split 3,1",
    );
    expect(engineLayout(engine({ gpuLayers: -1 }))).toBe("GPU layers automatic · context 8192 per request · 1 slot");
    expect(engineLayout(engine({ gpuLayers: 20, layers: 0 }))).toBe("20 GPU layers · context 8192 per request · 1 slot");
    expect(engineQueue(engine())).toBe("1 running · 0 chat and 2 background waiting");
  });

  it("writes an event as a line and marks failures", () => {
    const at = new Date(2026, 8, 25, 14, 2, 31).toISOString();
    expect(eventLine({ kind: "model_loaded", modelId: "qwen3-8b-q4km", detail: "port=41500", at })).toBe("14:02:31  model_loaded  qwen3-8b-q4km  port=41500");
    expect(eventLine({ kind: "engine_installed", modelId: null, detail: null, at })).toBe("14:02:31  engine_installed");
    expect(eventFailed({ kind: "engine_crashed", modelId: null, detail: null, at })).toBe(true);
    expect(eventFailed({ kind: "download_failed", modelId: null, detail: null, at })).toBe(true);
    expect(eventFailed({ kind: "model_evicted", modelId: null, detail: null, at })).toBe(false);
    expect(downloadPercent(0.429)).toBe("42%");
  });
});
