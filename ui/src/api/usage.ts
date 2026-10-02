/** Usage statistics (src-tauri/src/commands/usage.rs, nook_core::usage). */
import { call } from "./ipc";

/** One tool's uses since the last report, and how many of them failed. */
export interface ToolCount {
  uses: number;
  failed: number;
}

/** A report exactly as it is sent. Keys of `tools` are "<tool>.<action>", e.g. "pdf.save". */
export interface UsageReport {
  install: string;
  version: string;
  os: string;
  channel: string;
  gpu?: { vendor: string; vramGb?: number };
  tools: Record<string, ToolCount>;
}

export interface UsageOverview {
  enabled: boolean;
  /** False for a build from source or for a test feed: it never sends a report. */
  sends: boolean;
  /** ISO time of the last report the server took. */
  lastSent: string | null;
  next: UsageReport;
}

export const usageOverview = () => call<UsageOverview>("usage_overview");
/** Off drops the counts that were waiting. */
export const usageSet = (enabled: boolean) => call<void>("usage_set", { enabled });
/** The notice has been on screen; reports may go from now on. */
export const usageNoticeSeen = () => call<void>("usage_notice_seen");

/** Whether the hub shows the one-time notice: reports on, and the notice not shown yet. */
export function usageNoticeDue(settings: Record<string, string>): boolean {
  return settings.SHARE_USAGE?.trim().toLowerCase() === "true" && settings.USAGE_NOTICE_SHOWN?.trim().toLowerCase() !== "true";
}

/** The row's words under the switch. */
export function usageDescription(enabled: boolean): string {
  return enabled
    ? "Once a day: how often each tool was used and whether it worked, the app version, Windows or macOS and the graphics card's maker and memory, under a random install number. Never your files, what you type or say, or anything that names you."
    : "Off: nothing is counted or sent.";
}

/** The "What is sent" row's words: whether this build sends at all, and when it last did. */
export function usageSentText(o: Pick<UsageOverview, "enabled" | "sends" | "lastSent">, now = new Date()): string {
  if (!o.sends) return "This build sends no reports; only published builds do.";
  if (!o.enabled) return "Nothing, while the switch is off.";
  if (!o.lastSent) return "Nothing sent yet. This is the next report, exactly as it would go.";
  const last = new Date(o.lastSent);
  const sameDay = last.toDateString() === now.toDateString();
  const when = sameDay
    ? `today at ${last.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`
    : last.toLocaleDateString([], { day: "numeric", month: "long" });
  return `Last sent ${when}. This is the next report, exactly as it would go.`;
}
