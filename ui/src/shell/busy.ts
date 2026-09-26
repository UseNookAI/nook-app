/**
 * What would be cut short if the window closed now. The Kotlin app asked ModelDownloadService
 * (`downloadingModels.isNotEmpty()`) and VersionUpdateService (`isDownloading`) directly before it
 * closed; here each area reports into this registry and the close button asks it.
 *
 *   // in the Models page, while any model downloads:
 *   useBusyReporter(BusyKey.DOWNLOADS, downloading.length > 0);
 *
 * While any key is busy, closing the window asks first (the "Quit Nook?" dialog). The shell itself
 * reports BusyKey.UPDATE while the installer downloads.
 */
import { useEffect, useSyncExternalStore } from "react";

export const BusyKey = {
  /** Model or engine downloads; they carry on in the background after a quit. */
  DOWNLOADS: "downloads",
  /** The update installer is downloading; a quit cancels it. */
  UPDATE: "update",
} as const;

const busy = new Set<string>();
const listeners = new Set<() => void>();
let snapshot: readonly string[] = [];

function changed(): void {
  snapshot = [...busy].sort();
  listeners.forEach((fn) => fn());
}

/** Marks `key` busy or free. */
export function setBusy(key: string, isBusy: boolean): void {
  if (isBusy === busy.has(key)) return;
  if (isBusy) busy.add(key);
  else busy.delete(key);
  changed();
}

/** The busy keys right now. */
export function busyKeys(): readonly string[] {
  return snapshot;
}

/** Reports `key` busy while `isBusy` is true and the calling component is mounted. */
export function useBusyReporter(key: string, isBusy: boolean): void {
  useEffect(() => {
    setBusy(key, isBusy);
    return () => setBusy(key, false);
  }, [key, isBusy]);
}

/** The busy keys, re-rendering when they change. */
export function useBusyKeys(): readonly string[] {
  return useSyncExternalStore(
    (fn) => {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    () => snapshot,
  );
}
