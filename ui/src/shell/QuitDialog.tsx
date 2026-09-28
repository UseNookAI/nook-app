/**
 * "Quit Nook?" (NookAgentApplication.App): asked when the window closes while model downloads or
 * the update download run, and (new here) while anything would be lost or stopped: edited files
 * or PDFs not saved, a Nooklet or a render running (`reasons`). Downloads carry on in the
 * background; a running update download stops.
 */
import { Button } from "../components/Button";
import { Dialog } from "../components/Dialog";
import { BusyKey } from "./busy";

export function quitMessage(busy: readonly string[]): string {
  const updating = busy.includes(BusyKey.UPDATE);
  const others = busy.some((k) => k !== BusyKey.UPDATE);
  if (updating && others) {
    return "Version update and model downloads are in progress. If you quit, the version update will be cancelled, but model downloads will continue in the background.";
  }
  if (updating) return "A version update is being downloaded. If you quit now, the download will be stopped.";
  return "Model downloads continue in the background. If unfinished, click download to resume.";
}

/** "A PDF has changes…": the core's and the pages' words, each starting with a capital. */
export function reasonText(reason: string): string {
  return reason.charAt(0).toUpperCase() + reason.slice(1);
}

export function QuitDialog({
  busy,
  reasons = [],
  onDismiss,
  onConfirm,
}: {
  busy: readonly string[];
  /** What quitting would lose or stop, in words. */
  reasons?: readonly string[];
  onDismiss: () => void;
  onConfirm: () => void;
}) {
  const losing = reasons.length > 0;
  return (
    <Dialog
      title="Quit Nook?"
      onDismiss={onDismiss}
      actions={
        <>
          <Button text="Cancel" variant="ghost" onClick={onDismiss} />
          <Button text={losing ? "Quit anyway" : "Quit"} variant={losing ? "danger" : "primary"} onClick={onConfirm} />
        </>
      }
    >
      {losing && (
        <>
          <div>If you quit now:</div>
          <ul className="nk-quit__reasons">
            {reasons.map((r) => (
              <li key={r}>{reasonText(r)}.</li>
            ))}
          </ul>
        </>
      )}
      {busy.length > 0 && quitMessage(busy)}
    </Dialog>
  );
}
