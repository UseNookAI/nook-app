/**
 * "Quit Nook?" (NookAgentApplication.App): asked when the window closes while model downloads or
 * the update download run. Downloads carry on in the background; a running update download stops.
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

export function QuitDialog({ busy, onDismiss, onConfirm }: { busy: readonly string[]; onDismiss: () => void; onConfirm: () => void }) {
  return (
    <Dialog
      title="Quit Nook?"
      onDismiss={onDismiss}
      actions={
        <>
          <Button text="Cancel" variant="ghost" onClick={onDismiss} />
          <Button text="Quit" variant="primary" onClick={onConfirm} />
        </>
      }
    >
      {quitMessage(busy)}
    </Dialog>
  );
}
