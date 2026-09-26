import { mock, mockEmit } from "../ipc";
import type { Release, UpdateChannel, UpdateStatus } from "../update";
import { mockFlag } from "./app";

const status: UpdateStatus = {
  enabled: true,
  currentVersion: "0.5.0",
  buildLabel: "0.5.0 (mock, 2026-09-25)",
  channel: "stable",
  isUpdateAvailable: false,
  latestVersionInfo: null,
  checking: false,
  isDownloading: false,
  downloadProgress: 0,
  updateError: null,
  lastCheckError: null,
  waitingNote: null,
  snoozed: false,
};

const release = (channel: UpdateChannel): Release => ({
  channel,
  version: channel === "dev" ? "0.5.1-dev.42" : "0.5.1",
  file: "Nook-0.5.1.exe",
  url: "https://dl.usenook.ai/nook/builds/0.5.1-abc1234/Nook-0.5.1.exe",
  sha256: "0".repeat(64),
  size: 48_000_000,
  commit: "f00dcafe",
  notes: "Faster model loading and a steadier Code worker.",
  published: new Date().toISOString(),
  title: `${channel === "dev" ? "0.5.1-dev.42" : "0.5.1"} (f00dcafe)`,
});

let timer: number | undefined;

function emit(): void {
  mockEmit("update", { ...status });
}

function check(): void {
  window.setTimeout(() => {
    const offer = mockFlag("update") !== null || status.channel === "dev";
    status.lastCheckError = null;
    if (offer) {
      if (!status.isUpdateAvailable) status.snoozed = false;
      status.latestVersionInfo = release(status.channel);
      status.isUpdateAvailable = true;
    } else if (!status.isDownloading) {
      status.latestVersionInfo = null;
      status.isUpdateAvailable = false;
    }
    emit();
  }, 600);
}

export function registerUpdateMocks(): void {
  mock("update_status", () => ({ ...status }));
  mock("update_check", () => check());
  mock("update_start", () => {
    if (!status.latestVersionInfo || status.isDownloading) return;
    status.isDownloading = true;
    status.downloadProgress = 0;
    status.updateError = null;
    emit();
    window.clearInterval(timer);
    timer = window.setInterval(() => {
      status.downloadProgress = Math.min(1, status.downloadProgress + 0.02);
      if (status.downloadProgress >= 1) {
        window.clearInterval(timer);
        status.isDownloading = false;
        status.updateError = "Update failed: the browser preview cannot run the installer.";
      }
      emit();
    }, 300);
  });
  mock("update_cancel", () => {
    window.clearInterval(timer);
    status.isDownloading = false;
    status.downloadProgress = 0;
    emit();
  });
  mock("update_snooze", () => {
    status.snoozed = true;
    emit();
  });
  mock("update_set_channel", ({ channel }) => {
    status.channel = channel as UpdateChannel;
    status.latestVersionInfo = null;
    status.isUpdateAvailable = false;
    emit();
    check();
  });
}
