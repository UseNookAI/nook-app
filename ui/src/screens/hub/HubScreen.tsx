/**
 * Everything under the title strip (HubScreen.kt): Nook Code (`CodeHub`). On the first start Nook
 * loads the installed models behind a small card over the whole window, then asks for an update
 * check, and when models are installed without the engine installs it in the background with a
 * snackbar at the end.
 */
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { messageOf } from "../../api/ipc";
import { runtimeEnsureEngine, runtimeInit, runtimeRefreshModels } from "../../api/runtime";
import { updateCheck } from "../../api/update";
import { useSnackbar } from "../../components/Snackbar";
import { Spinner } from "../../components/Spinner";
import { CodeHub } from "../code/CodeHub";
import "./hub.css";

export interface HubScreenProps {
  isSidebarCollapsed: boolean;
  onToggleSidebar: () => void;
  /** Opens Settings on a tab: "general", "models", "models/code", "runtime", "about". */
  onSettings: (tabId: string) => void;
}

export function HubScreen({ isSidebarCollapsed, onToggleSidebar, onSettings }: HubScreenProps) {
  const { say } = useSnackbar();
  const [initializing, setInitializing] = useState(true);
  const [initStatus, setInitStatus] = useState("Starting Nook...");
  const started = useRef(false);

  useEffect(() => {
    if (started.current) return;
    started.current = true;
    (async () => {
      // A first start with no model installed does not download the runtime behind an overlay
      // (645 MB for CUDA; QA of 2026-09-23): the first model download installs it
      // (RuntimeManager.download). Models already installed without it (a backend switch, a
      // deleted runtime) bring it back in the background instead.
      const init = await runtimeInit().catch(() => ({ needsEngine: false, backendLabel: "" }));
      setInitStatus("Loading installed models...");
      await runtimeRefreshModels().catch((e) => console.warn(`Models not refreshed: ${messageOf(e)}`));
      setInitializing(false);
      updateCheck().catch(() => {});
      if (init.needsEngine) {
        let message: string;
        try {
          const ok = await runtimeEnsureEngine();
          message = ok ? `The Nook runtime (${init.backendLabel}) is installed.` : "The Nook runtime install stopped.";
        } catch (e) {
          message = `Runtime install failed: ${messageOf(e)}`;
        }
        say(message, true);
      }
    })();
  }, [say]);

  return (
    <div className="nk-hub">
      <CodeHub isSidebarCollapsed={isSidebarCollapsed} onToggleSidebar={onToggleSidebar} onSettings={onSettings} />
      {initializing && <StartingCard status={initStatus} />}
    </div>
  );
}

/** The first-start card over the whole window, which takes no clicks while it shows. */
function StartingCard({ status }: { status: string }) {
  return createPortal(
    <div className="nk-starting" onMouseDown={(e) => e.preventDefault()} onClick={(e) => e.stopPropagation()}>
      <div className="nk-starting__card" role="status">
        <Spinner size={30} stroke={3} />
        <div className="body1">{status}</div>
      </div>
    </div>,
    document.body,
  );
}
