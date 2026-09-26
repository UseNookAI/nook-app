/**
 * The folder's session with the worker as a narrow chat column: the same thread and composer as
 * the Chat page, used by the Code page's Nook panel (ide/AssistantPanel.kt, below its header).
 * A request carries the open file and selection ([editorContext]), and an Apply tells the editor
 * to read its files again.
 */
import { useEffect, useRef, useState } from "react";
import { codeStart, type EditorContext } from "../../api/code";
import { repoName } from "../../components/paths";
import { CodeComposer } from "./CodeComposer";
import { SessionComposer, SessionThread, useRepositoryRefusal } from "./CodeSessionScreen";
import { useCodeActions, useCodeSnapshot } from "./useCode";
import "./code.css";

export interface SessionColumnProps {
  /** The folder open in the Code page; null when none. */
  folder: string | null;
  /** The session this folder's panel uses, or null before the first request. */
  sessionId: string | null;
  /** Called with a new session's id after the first request starts it. */
  onSessionStarted: (id: string) => void;
  /** The open file and selection, sent with each request (null when the person turned it off). */
  editorContext: EditorContext | null;
  /** After Apply, so the editor reloads open files. */
  onApplied: () => void;
  onOpenModels: () => void;
}

export function SessionColumn({ folder, sessionId, onSessionStarted, editorContext, onApplied, onOpenModels }: SessionColumnProps) {
  const snapshot = useCodeSnapshot();
  const actions = useCodeActions();
  const session = sessionId == null ? null : (snapshot.sessions.find((s) => s.id === sessionId) ?? null);

  // An Apply changes the files on disk: the open ones read again, the explorer too.
  let applied: string | null = null;
  if (session) {
    for (let i = session.entries.length - 1; i >= 0; i--) {
      const e = session.entries[i];
      if (e.kind === "note" && e.tone === "applied") {
        applied = e.id;
        break;
      }
    }
  }
  const onAppliedRef = useRef(onApplied);
  onAppliedRef.current = onApplied;
  useEffect(() => {
    if (applied != null) onAppliedRef.current();
  }, [applied]);

  if (folder == null) {
    return (
      <div className="nc-column">
        <div className="body2 text-tertiary nc-column__hint">
          Open a folder, then ask Nook for a change in it here. The worker changes a private copy; you read the diff and apply it.
        </div>
      </div>
    );
  }
  if (session == null) {
    return (
      <ColumnStart
        key={folder}
        folder={folder}
        workerName={snapshot.workerName}
        workerHint={snapshot.workerHint}
        editorContext={editorContext}
        onStarted={onSessionStarted}
        onOpenModels={onOpenModels}
      />
    );
  }
  return (
    <div className="nc-column">
      <SessionThread session={session} snapshot={snapshot} actions={actions} compact />
      <div className="nc-column__composer">
        <SessionComposer session={session} snapshot={snapshot} actions={actions} compact editorContext={editorContext} />
      </div>
    </div>
  );
}

/** No session for this folder yet: the first request starts one, as the Chat page does. */
function ColumnStart({
  folder,
  workerName,
  workerHint,
  editorContext,
  onStarted,
  onOpenModels,
}: {
  folder: string;
  workerName: string | null;
  workerHint: string;
  editorContext: EditorContext | null;
  onStarted: (id: string) => void;
  onOpenModels: () => void;
}) {
  const actions = useCodeActions();
  const [text, setText] = useState("");
  const [starting, setStarting] = useState(false);
  const refused = useRepositoryRefusal(folder);
  return (
    <div className="nc-column">
      <div className="nc-column__start">
        <div className="subtitle2">Ask for a change in {repoName(folder) || folder}.</div>
        <div className="caption text-tertiary nc-column__lead">
          The local worker makes it in a private copy of the folder and shows the diff here. Nothing touches your files until you apply it.
        </div>
        {refused != null && <div className="caption text-warning nc-column__note">{refused}</div>}
        {workerName == null && (
          <div className="caption text-warning nc-column__note">
            Nook needs a worker model first. Download {workerHint} in{" "}
            <button type="button" className="nc-inline-link" onClick={onOpenModels}>
              Settings &gt; Models
            </button>
            .
          </div>
        )}
      </div>
      <div className="nc-column__composer">
        <CodeComposer
          text={text}
          onTextChange={setText}
          placeholder="Describe the change"
          repository={folder}
          repoLocked
          repositories={[]}
          onRepository={() => undefined}
          workerName={workerName}
          running={starting}
          repositoryReady={refused == null}
          onNotice={actions.say}
          onSend={() => {
            const t = text;
            setStarting(true);
            actions.run(async () => {
              try {
                const s = await codeStart(folder, t, null, editorContext);
                setText("");
                onStarted(s.id);
              } finally {
                setStarting(false);
              }
            });
          }}
          onStop={() => undefined}
          showRepository={false}
        />
      </div>
    </div>
  );
}
