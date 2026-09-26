/**
 * The files of the open folder as a tree (Explorer.kt): folders open and close (listed the first
 * time they open), files open in the editor, and a right-click offers new file, new folder,
 * rename and delete, which the page asks about first.
 */
import { useMemo, useState } from "react";
import { ToolbarIcon } from "../../components/Button";
import { Menu } from "../../components/Menu";
import { baseName } from "../../components/paths";
import type { FileNode } from "../../api/ide";
import { parentDir, treeRows, type TreeRow } from "./IdeSupport";
import type { IdeWorkspace } from "./IdeWorkspace";

/** What the explorer's menu asks the page to do; the page asks for a name or a confirmation first. */
export type ExplorerAction =
  | { kind: "newFile"; dir: string }
  | { kind: "newFolder"; dir: string }
  | { kind: "rename"; path: string }
  | { kind: "delete"; node: FileNode };

export function Explorer({
  ws,
  width,
  onAction,
}: {
  ws: IdeWorkspace;
  width: number;
  onAction: (action: ExplorerAction) => void;
}) {
  const root = ws.folder;
  const { expanded, children } = ws;
  const rows = useMemo(() => (root ? treeRows(root, expanded, children) : []), [root, expanded, children]);
  const [menu, setMenu] = useState<{ row: TreeRow; x: number; y: number } | null>(null);
  if (!root) return null;
  const activePath = ws.active?.path;

  /** Where a new file goes: the selected folder, the selected file's folder, or the root. */
  const folderBeside = () => {
    const s = ws.selected;
    if (!s) return root;
    return ws.selectedIsDir ? s : (parentDir(s) ?? root);
  };

  return (
    <div className="ide-explorer" style={{ width }}>
      <div className="ide-pane-header">
        <span className="ide-pane-header__title body2 ide-ellipsis">{baseName(root)}</span>
        <ToolbarIcon icon="plus" hint="New file" onClick={() => onAction({ kind: "newFile", dir: folderBeside() })} />
        <ToolbarIcon icon="refresh" hint="Read the folder again" onClick={() => void ws.refresh()} />
      </div>
      <div className="ide-tree">
        {rows.length === 0 && <div className="ide-tree__empty caption">This folder is empty.</div>}
        {rows.map((row) => {
          const { node } = row;
          const classes = [
            "ide-row",
            node.path === ws.selected ? "ide-row--selected" : "",
            node.path === activePath ? "ide-row--active" : "",
          ]
            .filter(Boolean)
            .join(" ");
          return (
            <div
              key={node.path}
              className={classes}
              style={{ paddingLeft: 8 + 14 * row.depth }}
              onClick={() => {
                ws.select(node.path, node.isDirectory);
                if (node.isDirectory) ws.toggle(node.path);
                else void ws.open(node.path);
              }}
              onContextMenu={(e) => {
                e.preventDefault();
                ws.select(node.path, node.isDirectory);
                setMenu({ row, x: e.clientX, y: e.clientY });
              }}
            >
              {node.isDirectory ? <Chevron open={expanded.has(node.path)} /> : <FileGlyph />}
              <span className="ide-row__name body2">{node.name}</span>
            </div>
          );
        })}
      </div>
      {menu && (
        <Menu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={(() => {
            const { node } = menu.row;
            const dir = node.isDirectory ? node.path : (parentDir(node.path) ?? root);
            return [
              { label: "New file…", icon: "plus", onSelect: () => onAction({ kind: "newFile", dir }) },
              { label: "New folder…", icon: "folder-plus", onSelect: () => onAction({ kind: "newFolder", dir }) },
              { label: "Rename…", icon: "pencil-edit", onSelect: () => onAction({ kind: "rename", path: node.path }) },
              { label: "Delete", icon: "trash", danger: true, onSelect: () => onAction({ kind: "delete", node }) },
            ];
          })()}
        />
      )}
    </div>
  );
}

/** A small arrow, pointing right at a closed folder and down at an open one. */
function Chevron({ open }: { open: boolean }) {
  return (
    <span className={open ? "ide-chevron ide-chevron--open" : "ide-chevron"}>
      <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
        <path d="M4.32 2.64 7.92 6 4.32 9.36" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    </span>
  );
}

/** A sheet with a turned corner, for a file. */
function FileGlyph() {
  // The original's path on an 11 px square: fold = 32% of the width.
  return (
    <span className="ide-file-glyph">
      <svg width="11" height="11" viewBox="0 0 11 11" fill="none">
        <path
          d="M1.32 0.55 H6.16 L9.68 4.07 V10.45 H1.32 Z M6.16 0.55 V4.07 H9.68"
          stroke="currentColor"
          strokeWidth="1.1"
          strokeLinejoin="round"
        />
      </svg>
    </span>
  );
}
