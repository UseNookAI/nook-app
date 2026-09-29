/**
 * The update dialog (NookAgentApplication.App): the offer with "Later" and "Update Now", then the
 * download with its progress and "Cancel". It cannot be dismissed from outside.
 */
import type { Release, UpdateStatus } from "../api/update";
import { Button } from "../components/Button";
import { Dialog } from "../components/Dialog";
import { ProgressBar } from "../components/Spinner";
import "./shell.css";
import { isMac } from "./platform";

/** The update dialog's sentence: the build, its notes when it has any, and what happens next. */
export function updateOffer(release: Release | null): string {
  const notes = release?.notes?.trim() ? release.notes : null;
  const what = notes == null ? "." : `: ${notes}`;
  return `Nook ${release?.title ?? ""} is available${what} Update now? The app closes, installs and starts again.`;
}

export function UpdateDialog({
  status,
  onLater,
  onUpdateNow,
  onCancel,
}: {
  status: UpdateStatus;
  onLater: () => void;
  onUpdateNow: () => void;
  onCancel: () => void;
}) {
  const downloading = status.isDownloading;
  return (
    <Dialog
      width={480}
      title={downloading ? "Downloading Update" : "Update Available"}
      actions={
        downloading ? (
          <Button text="Cancel" variant="ghost" onClick={onCancel} />
        ) : (
          <>
            <Button text="Later" variant="ghost" onClick={onLater} />
            <Button text="Update Now" variant="primary" onClick={onUpdateNow} />
          </>
        )
      }
    >
      {downloading
        ? isMac
          ? "Please wait while the new version of Nook is being downloaded. Nook will close, update itself and open again once finished."
          : "Please wait while the new version of Nook is being downloaded. The installer will launch automatically once finished."
        : updateOffer(status.latestVersionInfo)}
      {downloading && (
        <>
          <div className="nk-update__progress">
            <ProgressBar progress={status.downloadProgress} height={8} />
          </div>
          <div className="caption nk-update__percent">Downloading: {Math.trunc(status.downloadProgress * 100)}%</div>
        </>
      )}
      {status.updateError && <div className="caption nk-update__error">{status.updateError}</div>}
    </Dialog>
  );
}
