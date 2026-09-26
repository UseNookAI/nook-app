/** Settings › Runtime's wording, kept pure for the tests (RuntimeSettingsView.kt). */
import { ago } from "../../../components/activityFormat";
import { fastEnoughToWork, type EngineInfo, type GpuDevice, type RuntimeEvent, type SpeedProbeResult } from "../../../api/runtime";

const GB = 1024 * 1024 * 1024;

/** A Java double as Kotlin's string template prints it: "31.4", and "31.0" for a whole number. */
export function javaDouble(v: number): string {
  return Number.isInteger(v) ? v.toFixed(1) : String(v);
}

/** "3.1 of 8 GB used". */
export function gpuUsedText(gpu: GpuDevice): string {
  const used = Math.max(0, gpu.totalBytes - gpu.freeBytes) / GB;
  return `${used.toFixed(1)} of ${(gpu.totalBytes / GB).toFixed(0)} GB used`;
}

/** The share of the card in use, 0..1, and whether it is nearly full (over 90 %). */
export function gpuUsedShare(gpu: GpuDevice): { share: number; nearlyFull: boolean } {
  const share = gpu.totalBytes > 0 ? Math.max(0, gpu.totalBytes - gpu.freeBytes) / gpu.totalBytes : 0;
  return { share: Math.min(1, share), nearlyFull: share > 0.9 };
}

/** "Driver 581.29 · 4.6 GB available for models". */
export function gpuDriverText(gpu: GpuDevice, budgetBytes: number): string {
  return `Driver ${gpu.driverVersion ?? "unknown"} · ${(budgetBytes / GB).toFixed(1)} GB available for models`;
}

/** A probe as its row's title: "gpt-oss 20B · 31.4 tokens per second". */
export function probeTitle(p: SpeedProbeResult, engines: readonly EngineInfo[]): string {
  const name = engines.find((e) => e.modelId === p.modelId)?.displayName ?? p.modelId;
  return `${name} · ${javaDouble(p.generateTps)} tokens per second`;
}

/** "Reads 412 tokens per second · fast enough for Nook Code · measured 2 h ago with driver 581.29". */
export function probeDescription(p: SpeedProbeResult, now: number = Date.now()): string {
  const verdict = fastEnoughToWork(p) ? "fast enough for Nook Code" : "too slow for Nook Code (under 8 tokens per second)";
  return `Reads ${Math.trunc(p.promptTps)} tokens per second · ${verdict} · measured ${ago(Date.parse(p.measuredAt), now)} with driver ${p.driver}`;
}

/** "Ready", "Starting": an engine state as its chip. */
export function stateText(state: string): string {
  const lower = state.toLowerCase();
  return lower.charAt(0).toUpperCase() + lower.slice(1);
}

/** Where a loaded model's layers are and what it serves: "24/24 layers on GPU · context 8192 per request · 1 slot". */
export function engineLayout(e: EngineInfo): string {
  const layers =
    e.gpuLayers < 0
      ? "GPU layers automatic"
      : e.layers > 0
        ? `${Math.min(e.gpuLayers, e.layers)}/${e.layers} layers on GPU`
        : `${e.gpuLayers} GPU layers`;
  const split = e.tensorSplit != null ? ` · split ${e.tensorSplit}` : "";
  return `${layers} · context ${e.ctxPerSlot} per request · ${e.slots} slot${e.slots === 1 ? "" : "s"}${split}`;
}

/** "1 running · 0 chat and 2 background waiting". */
export function engineQueue(e: EngineInfo): string {
  return `${e.active} running · ${e.waitingInteractive} chat and ${e.waitingBackground} background waiting`;
}

const pad = (n: number) => String(n).padStart(2, "0");

/** "14:02:31  model_loaded  qwen3-8b-q4km  port=41500 slots=1": one line of the event feed, local time. */
export function eventLine(ev: RuntimeEvent): string {
  const d = new Date(ev.at);
  const time = `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  return `${time}  ${ev.kind}  ${ev.modelId ?? ""}  ${ev.detail ?? ""}`.trim();
}

/** A failure or a crash, drawn in the error colour. */
export function eventFailed(ev: RuntimeEvent): boolean {
  return ev.kind.includes("fail") || ev.kind.includes("crash");
}

/** A download's percent as the Downloads group shows it: "42%", truncated. */
export function downloadPercent(p: number): string {
  return `${Math.trunc(p * 100)}%`;
}
