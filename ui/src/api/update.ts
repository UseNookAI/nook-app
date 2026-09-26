/**
 * Self-update (nook_core::update, src-tauri/src/commands/update.rs). Ports what the UI read from
 * `service/VersionUpdateService.kt` and `SettingsViewModel.kt`: the field names are the Kotlin ones.
 *
 * The core checks at start and on its own schedule; the UI asks for a check once the hub is up, on
 * Settings › General › Check and after a channel change (update_set_channel checks by itself). Every
 * change of state is pushed as a full [UpdateStatus] on the "update" topic.
 */
import { call, on } from "./ipc";

/** `update/ReleaseManifest.Release`: one signed build on a channel. */
export interface Release {
  channel: string;
  version: string;
  file: string;
  url: string;
  sha256: string;
  size: number;
  commit: string;
  notes: string;
  /** ISO-8601 instant, or null when the manifest had none. */
  published: string | null;
  /** `Release.title()`: "0.5.1 (abc1234)", or the version alone when there is no commit. */
  title: string;
}

export type UpdateChannel = "stable" | "dev";

export interface UpdateStatus {
  /** `UpdateSource.enabled()`: false in a dev build with no feed configured; checks do nothing. */
  enabled: boolean;
  /** `currentVersion`: "0.5.0". */
  currentVersion: string;
  /** `buildLabel`: "0.5.0 (abc1234, 2026-09-25)" for About. */
  buildLabel: string;
  channel: UpdateChannel;
  isUpdateAvailable: boolean;
  /** The build on offer, or null. */
  latestVersionInfo: Release | null;
  /** A check is under way (updater.rs `UpdateStatus.checking`). */
  checking: boolean;
  isDownloading: boolean;
  /** 0..1 while the installer downloads. */
  downloadProgress: number;
  /** "Update failed: ..." after a download or verification failed; null otherwise. */
  updateError: string | null;
  /** What the last check found wrong, for Settings; null when it went through or never ran. */
  lastCheckError: string | null;
  /** Dev channel: a build found and held back while something runs ("Nook 0.5.1 installs once..."). */
  waitingNote: string | null;
  /**
   * The person answered "Later" to the offer on screen. Cleared when a new offer arrives (the
   * Kotlin view model showed the dialog again whenever isUpdateAvailable turned true).
   */
  snoozed: boolean;
}

export const updateStatus = () => call<UpdateStatus>("update_status");
/** Starts a check in the background; the result arrives on the "update" topic. */
export const updateCheck = () => call<void>("update_check");
/** Downloads, verifies and runs the installer of `latestVersionInfo`; the app exits when it starts. */
export const updateStart = () => call<void>("update_start");
/** Stops the installer download and deletes the partial file. */
export const updateCancel = () => call<void>("update_cancel");
/** "Later": hides the offer until a new one arrives. */
export const updateSnooze = () => call<void>("update_snooze");
/** Saves the channel (UPDATE_CHANNEL), drops the other channel's offer and checks again. */
export const updateSetChannel = (channel: UpdateChannel) => call<void>("update_set_channel", { channel });

/** Every change of the update state, as a full snapshot. */
export const onUpdate = (handler: (status: UpdateStatus) => void) => on<UpdateStatus>("update", handler);
