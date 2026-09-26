/**
 * CodeSidebar.kt (with hub/sidebar/SidebarItem.kt and SidebarIconButton.kt): the sessions so far,
 * and Settings. Chat, Code and Flows are in the title strip (HubNav).
 */
import { useEffect, useRef, useState } from "react";
import type { CodeSession } from "../../api/code";
import { LiveDot } from "../../components/Activity";
import { Menu } from "../../components/Menu";
import { repoName } from "../../components/paths";
import { SidebarIconButton, SidebarItem } from "../../components/SidebarItem";
import { isRunning } from "./useCode";
import "./code.css";

/**
 * The sidebar: the sessions so far (the open one, `selectedId`, lit), and Settings. A session
 * renames from its right-click menu (or a double click) and deletes from the bin on hover.
 */
export function CodeSidebar({
  sessions,
  selectedId,
  isCollapsed,
  onOpen,
  onDelete,
  onRename,
  onSettings,
}: {
  sessions: CodeSession[];
  selectedId: string | null;
  isCollapsed: boolean;
  onOpen: (id: string) => void;
  onDelete: (id: string) => void;
  onRename: (id: string, title: string) => void;
  onSettings: () => void;
}) {
  return (
    <nav className={isCollapsed ? "nc-sidebar nc-sidebar--collapsed" : "nc-sidebar"}>
      {!isCollapsed ? (
        <>
          <div className="overline text-tertiary nc-sidebar__heading">Sessions</div>
          <div className="nc-sidebar__sessions">
            {sessions.length === 0 && <div className="body2 text-tertiary nc-sidebar__empty">Sessions you start appear here.</div>}
            {sessions.map((s) => (
              <CodeSessionItem
                key={s.id}
                session={s}
                isActive={s.id === selectedId}
                onClick={() => onOpen(s.id)}
                onDelete={() => onDelete(s.id)}
                onRename={(title) => onRename(s.id, title)}
              />
            ))}
          </div>
        </>
      ) : (
        <div className="nc-flex-spacer" />
      )}

      <SidebarItem icon="settings" label="Settings" isCollapsed={isCollapsed} onClick={onSettings} />
    </nav>
  );
}

function CodeSessionItem({
  session,
  isActive,
  onClick,
  onDelete,
  onRename,
}: {
  session: CodeSession;
  isActive: boolean;
  onClick: () => void;
  onDelete: () => void;
  onRename: (title: string) => void;
}) {
  const running = isRunning(session);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const [editing, setEditing] = useState(false);
  return (
    <div
      role="button"
      tabIndex={0}
      className={[
        "nc-session-item",
        isActive ? "nc-session-item--active" : "",
        running ? "nc-session-item--running" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      onClick={() => {
        if (!editing) onClick();
      }}
      onKeyDown={(e) => {
        if (!editing && (e.key === "Enter" || e.key === " ")) {
          e.preventDefault();
          onClick();
        }
        if (!editing && e.key === "F2") setEditing(true);
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        setMenu({ x: e.clientX, y: e.clientY });
      }}
    >
      <div className="nc-session-item__text">
        {editing ? (
          <RenameField
            initial={session.title}
            onDone={(title) => {
              setEditing(false);
              if (title != null && title.trim() && title.trim() !== session.title) onRename(title.trim());
            }}
          />
        ) : (
          <span className="body2 nc-session-item__title" onDoubleClick={() => setEditing(true)}>
            {session.title}
          </span>
        )}
        <span className="caption text-tertiary nc-ellipsis">{repoName(session.repository)}</span>
      </div>
      <div className="nc-session-item__end">
        {running ? (
          <LiveDot />
        ) : (
          <SidebarIconButton
            icon="trash"
            iconSize={14}
            title="Delete"
            className="nc-session-item__delete"
            onClick={(e) => {
              e.stopPropagation();
              onDelete();
            }}
          />
        )}
      </div>
      {menu && (
        <Menu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={[
            { label: "Rename", icon: "pencil-edit", onSelect: () => setEditing(true) },
            { label: "Delete", icon: "trash", danger: true, disabled: running, onSelect: onDelete },
          ]}
        />
      )}
    </div>
  );
}

/** The title in place, editable: Enter or leaving it keeps the new title, Escape the old one. */
function RenameField({ initial, onDone }: { initial: string; onDone: (title: string | null) => void }) {
  const [value, setValue] = useState(initial);
  const ref = useRef<HTMLInputElement>(null);
  const done = useRef(false);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  const finish = (title: string | null) => {
    if (done.current) return;
    done.current = true;
    onDone(title);
  };
  return (
    <input
      ref={ref}
      className="body2 nc-rename"
      value={value}
      maxLength={200}
      onChange={(e) => setValue(e.target.value)}
      onClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") finish(value);
        if (e.key === "Escape") finish(null);
      }}
      onBlur={() => finish(value)}
    />
  );
}
