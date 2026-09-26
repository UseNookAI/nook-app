/** The Video page's wording (VideoScreen.kt helpers), kept pure for the tests. */
import { clipSeconds, type Clip, type VideoSetup } from "../../api/video";

/** "2.1 s", "3 s", "12 s". */
export function seconds(s: number): string {
  return s >= 10 || s === Math.floor(s) ? `${s.toFixed(0)} s` : `${s.toFixed(1)} s`;
}

/** A duration in ms as "m:ss", or "h:mm:ss" from an hour. */
export function clock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const pad = (n: number) => String(n).padStart(2, "0");
  return total >= 3600
    ? `${Math.floor(total / 3600)}:${pad(Math.floor((total % 3600) / 60))}:${pad(total % 60)}`
    : `${Math.floor(total / 60)}:${pad(total % 60)}`;
}

export function gigabytes(bytes: number): string {
  return `${(bytes / 1e9).toFixed(1)} GB`;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** "d MMM, HH:mm" in English, local time. */
export function shortTime(epochMs: number): string {
  const d = new Date(epochMs);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getDate()} ${MONTHS[d.getMonth()]}, ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** The header's "832×480 · 2.1 s clips". */
export function clipText(setup: VideoSetup): string {
  return `${setup.width}×${setup.height} · ${seconds(setup.fps > 0 ? setup.frames / setup.fps : 0)} clips`;
}

/** What a running clip is doing, above its progress bar. */
export function stageText(clip: Clip): string {
  switch (clip.stage) {
    // Reading the weights takes seconds; encoding the prompt is most of this stage.
    case "LOADING":
      return "Reading the prompt";
    case "SAMPLING":
      return clip.total > 0 ? `Rendering · step ${clip.done} of ${clip.total}` : "Rendering";
    case "DECODING":
      return "Decoding the frames";
    case "SAVING":
      return "Saving";
    default:
      return "Starting";
  }
}

/** A finished clip's line: length, size, render time, seed and when. */
export function doneText(clip: Clip): string {
  return [
    seconds(clipSeconds(clip)),
    `${clip.width}×${clip.height}`,
    `made in ${clock(clip.elapsedMs)}`,
    `seed ${clip.seed}`,
    shortTime(clip.createdAt),
  ].join(" · ");
}
