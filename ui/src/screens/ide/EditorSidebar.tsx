/**
 * The sidebar on the Code page: the sessions started in its Nook panel (the one the panel shows
 * lit), then the folders worked in before. A session opens its folder with itself in the panel; a
 * folder opens as it is. The page asks about unsaved edits first (IdeWorkspace.requestOpen).
 */
import type { CodeSession } from "../../api/code";
import { repoName } from "../../components/paths";
import { CodeSessionItem } from "../code/CodeSidebar";
import { ideWorkspace, samePath, useWorkspace } from "./IdeWorkspace";
import { useRepositories } from "./useRepositories";

/** How many earlier folders the sidebar lists. */
const FOLDERS = 6;

export function EditorSidebar({
  sessions,
  onDelete,
  onRename,
}: {
  /** The Code page's sessions, newest first. */
  sessions: CodeSession[];
  onDelete: (id: string) => void;
  onRename: (id: string, title: string) => void;
}) {
  const ws = ideWorkspace();
  useWorkspace(ws);
  const repositories = useRepositories();
  const shown = ws.panelSession();

  return (
    <div className="nc-sidebar__scroll">
      <div className="overline text-tertiary nc-sidebar__heading">Sessions</div>
      <div className="nc-sidebar__list">
        {sessions.length === 0 && <div className="body2 text-tertiary nc-sidebar__empty">Changes you ask for beside the editor appear here.</div>}
        {sessions.map((s) => (
          <CodeSessionItem
            key={s.id}
            session={s}
            isActive={s.id === shown}
            onClick={() => ws.requestOpen(s.repository, s.id)}
            onDelete={() => onDelete(s.id)}
            onRename={(title) => onRename(s.id, title)}
          />
        ))}
      </div>
      {repositories.length > 0 && (
        <>
          <div className="overline text-tertiary nc-sidebar__heading">Folders</div>
          <div className="nc-sidebar__list">
            {repositories.slice(0, FOLDERS).map((r) => {
              const open = ws.folder != null && samePath(ws.folder, r);
              return (
                <button
                  key={r}
                  type="button"
                  className={open ? "nc-session-item nc-session-item--active" : "nc-session-item"}
                  title={r}
                  onClick={() => ws.requestOpen(r, null)}
                >
                  <span className="nc-session-item__text">
                    <span className="body2 nc-session-item__title">{repoName(r) || r}</span>
                    <span className="caption text-tertiary nc-ellipsis">{r}</span>
                  </span>
                </button>
              );
            })}
          </div>
        </>
      )}
    </div>
  );
}
