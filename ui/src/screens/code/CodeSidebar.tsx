/**
 * CodeSidebar.kt (with hub/sidebar/SidebarItem.kt and SidebarIconButton.kt): what the open page
 * has, and Settings. Chat, Code and Nooklets are in the title strip (HubNav), and the sidebar
 * follows them: the Chat page's chats, the Code page's sessions and folders (EditorSidebar), the
 * Nooklets and their recent runs (NookletsSidebar).
 */
import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { isEditorSession, type CodeSession } from "../../api/code";
import { LiveDot } from "../../components/Activity";
import { Menu } from "../../components/Menu";
import { repoName } from "../../components/paths";
import { SidebarIconButton, SidebarItem } from "../../components/SidebarItem";
import { NookletsSidebar } from "../flows/NookletsSidebar";
import { isRunning } from "./useCode";
import "./code.css";

// Loaded with the Code page, which it works with.
const EditorSidebar = lazy(() => import("../ide/EditorSidebar").then((m) => ({ default: m.EditorSidebar })));

/** The page the sidebar follows: the title strip's Chat, Code or Nooklets. */
export type SidebarSection = "chat" | "code" | "flows";

/**
 * The sidebar: the open page's list, and Settings. On the Chat page, the chats so far (the open
 * one, `selectedId`, lit); a session renames from its right-click menu (or a double click) and
 * deletes from the bin on hover, on the Code page too.
 */
export function CodeSidebar({
  section,
  sessions,
  selectedId,
  isCollapsed,
  onOpen,
  onDelete,
  onRename,
  onSettings,
}: {
  section: SidebarSection;
  sessions: CodeSession[];
  selectedId: string | null;
  isCollapsed: boolean;
  onOpen: (id: string) => void;
  onDelete: (id: string) => void;
  onRename: (id: string, title: string) => void;
  onSettings: () => void;
}) {
  let content;
  if (section === "flows") {
    content = <NookletsSidebar isCollapsed={isCollapsed} />;
  } else if (isCollapsed) {
    content = <div className="nc-flex-spacer" />;
  } else if (section === "code") {
    content = (
      <Suspense fallback={<div className="nc-flex-spacer" />}>
        <EditorSidebar sessions={sessions.filter(isEditorSession)} onDelete={onDelete} onRename={onRename} />
      </Suspense>
    );
  } else {
    const chats = sessions.filter((s) => !isEditorSession(s));
    content = (
      <>
        <div className="overline text-tertiary nc-sidebar__heading">Chats</div>
        <div className="nc-sidebar__sessions">
          {chats.length === 0 && <div className="body2 text-tertiary nc-sidebar__empty">Chats you start appear here.</div>}
          {chats.map((s) => (
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
    );
  }

  return (
    <nav className={isCollapsed ? "nc-sidebar nc-sidebar--collapsed" : "nc-sidebar"}>
      {content}
      <SidebarItem icon="settings" label="Settings" isCollapsed={isCollapsed} onClick={onSettings} />
    </nav>
  );
}

export function CodeSessionItem({
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
