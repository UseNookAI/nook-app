/**
 * The Nook panel beside the editor (AssistantPanel.kt): the folder's session with the local
 * worker as a narrow chat column (SessionColumn, the same thread and composer as the Chat page).
 * A request says which file is open and what is selected (the person can turn that off), and an
 * Apply reads the open files and the explorer again.
 */
import { memo, useCallback, useEffect, useMemo, useState } from "react";
import { codeSnapshot, onCodeChanged, type EditorContext } from "../../api/code";
import logo from "../../assets/images/nook-primary-logo.svg";
import { TextLink } from "../../components/Activity";
import { ToolbarIcon } from "../../components/Button";
import { SessionColumn } from "../code/SessionColumn";
import type { IdeWorkspace } from "./IdeWorkspace";

// The column re-renders only when what it is given changes, not on every caret move.
const Column = memo(SessionColumn);

export function AssistantPanel({
  ws,
  width,
  onOpenModels,
  onClose,
}: {
  ws: IdeWorkspace;
  width: number;
  onOpenModels: () => void;
  onClose: () => void;
}) {
  const folder = ws.folder;
  const stored = folder ? ws.sessionFor(folder) : null;
  const sessionId = useExistingSession(stored);

  const context = ws.editorContext();
  const contextKey = context ? JSON.stringify(context) : "";
  // The same context object until what it says changes.
  const editorContext = useMemo<EditorContext | null>(() => context, [contextKey]);

  const onSessionStarted = useCallback(
    (id: string) => {
      if (folder) ws.rememberSession(folder, id);
    },
    [ws, folder],
  );
  // An Apply changes the files on disk: the open ones read again, the explorer too.
  const onApplied = useCallback(() => {
    void ws.checkDisk();
    void ws.refresh();
  }, [ws]);

  return (
    <div className="ide-assistant" style={{ width }}>
      <div className="ide-pane-header">
        <img src={logo} alt="" className="ide-assistant__logo" />
        <span className="body2" style={{ fontWeight: 600 }}>
          Nook
        </span>
        <span className="ide-spacer" />
        <ToolbarIcon icon="close" hint="Hide the panel" onClick={onClose} />
      </div>
      {folder == null ? (
        <div className="ide-assistant__hint body2">
          Open a folder, then ask Nook for a change in it here. The worker changes a private copy; you read the diff and apply it.
        </div>
      ) : (
        <>
          <div className="ide-assistant__session">
            <Column
              folder={folder}
              sessionId={sessionId}
              onSessionStarted={onSessionStarted}
              editorContext={editorContext}
              onApplied={onApplied}
              onOpenModels={onOpenModels}
            />
          </div>
          <ContextLine ws={ws} />
        </>
      )}
    </div>
  );
}

/** Under the composer: what the request carries from the editor, and the switch to leave it out. */
function ContextLine({ ws }: { ws: IdeWorkspace }) {
  const tab = ws.active;
  if (!tab) return null;
  // Read on every render, which every caret move causes, so the line follows the selection.
  const lines = ws.selectedLines();
  return (
    <div className="ide-context-line">
      <span className="caption ide-context-line__text">
        {ws.shareOpenFile
          ? `Nook is told the open file is ${tab.name}` + (lines ? `, lines ${lines[0]}–${lines[1]}.` : ".")
          : "Nook is not told which file is open."}
      </span>
      <TextLink text={ws.shareOpenFile ? "Leave it out" : "Tell it"} onClick={() => ws.setShareOpenFile(!ws.shareOpenFile)} />
    </div>
  );
}

/**
 * The folder's session while it still exists: one deleted from the sidebar starts afresh, as the
 * original looked it up in the snapshot. When the sessions cannot be read, the id is trusted.
 */
function useExistingSession(id: string | null): string | null {
  const [gone, setGone] = useState<string | null>(null);
  useEffect(() => {
    if (!id) return;
    let alive = true;
    let timer: number | undefined;
    const check = () => {
      codeSnapshot()
        .then((s) => {
          if (alive) setGone(s.sessions.some((x) => x.id === id) ? null : id);
        })
        .catch(() => {
          if (alive) setGone(null);
        });
    };
    check();
    // Sessions change often while a turn runs; one look a second is plenty.
    const off = onCodeChanged(() => {
      window.clearTimeout(timer);
      timer = window.setTimeout(check, 1000);
    });
    return () => {
      alive = false;
      window.clearTimeout(timer);
      off();
    };
  }, [id]);
  return id && id !== gone ? id : null;
}
