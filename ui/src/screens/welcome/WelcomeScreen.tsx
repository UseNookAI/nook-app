/**
 * First-run screen (WelcomeScreen.kt). Nothing is installed from here any more: the app runs
 * entirely on the host, so the only job of this screen is to greet the user and mark setup as
 * complete.
 */
import { useState } from "react";
import { Setting, settingsSet } from "../../api/app";
import { messageOf } from "../../api/ipc";
import logo from "../../assets/images/nook-primary-logo.svg";
import { Button } from "../../components/Button";
import { useSnackbar } from "../../components/Snackbar";
import "./welcome.css";

/** Version of the welcome flow. Bump to show the welcome screen again after a breaking change. */
export const WELCOME_FLOW_VERSION = 1;

export function WelcomeScreen({ onProceed }: { onProceed: () => void }) {
  const [isSaving, setIsSaving] = useState(false);
  const { say } = useSnackbar();

  const getStarted = async () => {
    setIsSaving(true);
    try {
      // NookAgentService.saveSetupCompletionGlobalProperties(true, WELCOME_FLOW_VERSION)
      await settingsSet(Setting.IS_SETUP_COMPLETED, "true");
      await settingsSet(Setting.SETUP_VERSION, String(WELCOME_FLOW_VERSION));
      onProceed();
    } catch (e) {
      setIsSaving(false);
      say(`Could not save: ${messageOf(e)}`);
    }
  };

  return (
    <div className="nk-welcome">
      <div className="nk-welcome__column">
        <img src={logo} alt="Nook" className="nk-welcome__logo" draggable={false} />
        <h1 className="h2 nk-welcome__title">Your GPU, writing your code.</h1>
        <p className="body-large nk-welcome__body">
          Nook Code runs a coding model on this machine. Ask for a change in any repository: it works in a scratch copy, runs
          your checks and hands back a diff for you to read and apply. Nothing leaves this computer.
        </p>
        <Button text={isSaving ? "Starting…" : "Get started"} disabled={isSaving} onClick={getStarted} />
      </div>
    </div>
  );
}
