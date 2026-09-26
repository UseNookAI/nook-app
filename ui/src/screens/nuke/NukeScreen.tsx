/**
 * The erase screen (component/NukeScreen.kt), reached from Settings › General › Erase everything:
 * a card over a dark backdrop that lists what goes, and asks for "nuke" typed out before the red
 * button works. Erasing resets every setting and empties the log (`eraseEverything`), then Nook
 * quits, as the original's `exitProcess(0)` did; in the browser preview, where nothing quits, it
 * goes on to the welcome screen.
 */
import { useState } from "react";
import { eraseEverything, quit } from "../../api/app";
import { messageOf } from "../../api/ipc";
import { Button } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { TextField } from "../../components/TextField";
import "./nuke.css";

export function NukeScreen({ onProceed, onReturn }: { onProceed: () => void; onReturn: () => void }) {
  const [textInput, setTextInput] = useState("");
  const [isNuking, setIsNuking] = useState(false);
  const [nukingError, setNukingError] = useState<string | null>(null);
  const isNukeEnabled = textInput === "nuke" && !isNuking;

  const nuke = async () => {
    setIsNuking(true);
    setNukingError(null);
    try {
      await eraseEverything();
    } catch (e) {
      setNukingError(messageOf(e) || "Nuke operation failed.");
      setIsNuking(false);
      return;
    }
    await quit().catch(() => {});
    onProceed();
  };

  return (
    <div className="nk-nuke">
      <div className="nk-nuke__card" role="alertdialog" aria-labelledby="nk-nuke-title">
        <div className="nk-nuke__header">
          <Icon name="warning" size={20} />
          <span id="nk-nuke-title" className="h6 nk-nuke__title">
            Danger Zone
          </span>
        </div>
        <div className="nk-nuke__divider" />
        <div className="body2">Nuking the application will permanently erase:</div>
        <ol className="body2 nk-nuke__list">
          <li>1. Chat history</li>
          <li>2. Models downloaded</li>
          <li>3. Hardware/Benchmark reports</li>
          <li>4. Usage statistics</li>
          <li>5. Security keys</li>
        </ol>
        <div className="body2 nk-nuke__final">After this action, no recovery is possible.</div>
        <div className="caption nk-nuke__ask">
          Are you sure you want to do this? If so, please type in "nuke" (without quotes) in the following text box.
        </div>
        <TextField
          value={textInput}
          onChange={setTextInput}
          placeholder="Type 'nuke'"
          tone="danger"
          className="nk-nuke__input"
          autoFocus
          disabled={isNuking}
          onEnter={() => {
            if (isNukeEnabled) void nuke();
          }}
          onEscape={() => {
            if (!isNuking) onReturn();
          }}
        />
        {nukingError && <div className="caption nk-nuke__error">{nukingError}</div>}
        <div className="nk-nuke__actions">
          <Button text="Cancel" variant="ghost" disabled={isNuking} onClick={onReturn} />
          <Button text={isNuking ? "Nuking..." : "Nuke"} variant="danger" disabled={!isNukeEnabled} onClick={nuke} />
        </div>
      </div>
    </div>
  );
}
