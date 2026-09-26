/**
 * Tells the quit dialog while a model downloads (shell/busy, BusyKey.DOWNLOADS). The Kotlin window
 * asked ModelDownloadService and RuntimeManager (both BusyWork: "a model is downloading") when it
 * closed, whether or not Settings was open; here one listener on the "downloads" topic keeps the
 * key current for the whole life of the app, so a download started from Models and left running
 * after Settings closed still counts.
 */
import { modelsDownloads, onDownloads } from "../../../api/models";
import { runtimeDownloads } from "../../../api/runtime";
import { BusyKey, setBusy } from "../../../shell/busy";

let started = false;

/** Starts watching (once; later calls do nothing). */
export function watchDownloadsBusy(): void {
  if (started || typeof window === "undefined") return;
  started = true;
  let reading = false;
  let again = false;
  const read = () => {
    if (reading) {
      again = true;
      return;
    }
    reading = true;
    Promise.all([modelsDownloads().catch(() => null), runtimeDownloads().catch(() => null)])
      .then(([library, runtime]) => {
        // ModelDownloadService.busyWith: downloadingModels; RuntimeManager.busyWith: downloads().
        const busy = (library?.downloadingModels.length ?? 0) > 0 || Object.keys(runtime ?? {}).length > 0;
        setBusy(BusyKey.DOWNLOADS, busy);
      })
      .finally(() => {
        reading = false;
        if (again) {
          again = false;
          read();
        }
      });
  };
  onDownloads(read);
  // After the current task: in the browser the mocks are registered once main.tsx's imports ran.
  window.setTimeout(read, 0);
}
