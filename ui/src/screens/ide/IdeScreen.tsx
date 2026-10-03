/**
 * The Code page (IdeScreen.kt): a folder's files on the left, the open file in the editor in the
 * middle, and Nook on the right, to ask for changes in the same folder. The state is the
 * workspace (IdeWorkspace.ts), kept across visits and remembered in `<home>\code\ide.json`.
 */
import { useEffect, useState } from "react";
import { QuietAction } from "../../components/Activity";
import { Button, ToolbarIcon } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { baseName, repoName } from "../../components/paths";
import { useSnackbar } from "../../components/Snackbar";
import { chooseFolder } from "../../api/ide";
import { RepositoryPicker } from "../code/CodeComposer";
import { ToolbarPill } from "../hub/ComposerControls";
import { PagePane } from "../hub/PagePane";
import { AssistantPanel } from "./AssistantPanel";
import { EditorPane } from "./EditorPane";
import { Explorer, type ExplorerAction } from "./Explorer";
import { ConfirmDialog, NameDialog, UnsavedDialog } from "./IdeDialogs";
import { HairLine, ResizeHandle } from "./IdeParts";
import { isUnder } from "./IdeSupport";
import { clampPanel, ideWorkspace, samePath, useWorkspace, type EditorTab, type IdeWorkspace } from "./IdeWorkspace";
import { useRepositories } from "./useRepositories";
import "./ide.css";

export interface IdeScreenProps {
  onOpenModels: () => void;
}

/** A question the page is asking before it goes on: unsaved edits, a name, or a yes. */
type Ask =
  | { kind: "unsaved"; tabs: EditorTab[]; then: () => void }
  | { kind: "name"; title: string; initial: string; confirm: string; onConfirm: (name: string) => void }
  | { kind: "confirm"; title: string; body: string; confirm: string; onConfirm: () => void };

/** How often open files are compared with the disk while the page shows. */
const DISK_CHECK_MS = 2000;

export function IdeScreen({ onOpenModels }: IdeScreenProps) {
  const ws = ideWorkspace();
  useWorkspace(ws);
  const { say } = useSnackbar();
  const repositories = useRepositories();
  const [ask, setAsk] = useState<Ask | null>(null);

  // A file changed by an Apply or another program is read again while the page is open.
  useEffect(() => {
    const timer = window.setInterval(() => void ws.checkDisk(), DISK_CHECK_MS);
    return () => window.clearInterval(timer);
  }, [ws]);

  useEffect(() => {
    if (ws.notices.length > 0) ws.takeNotices().forEach((n) => say(n));
  });

  /** Runs `then` once the unsaved edits in `tabs`, if any, are saved or let go. */
  const withSaved = (tabs: EditorTab[], then: () => void) => {
    const dirty = tabs.filter((t) => t.modified);
    if (dirty.length === 0) then();
    else setAsk({ kind: "unsaved", tabs: dirty, then });
  };
  const openFolder = (path: string) => withSaved(ws.dirtyTabs(), () => void ws.openFolder(path));

  // What the sidebar asked for: its folder opens (unsaved edits asked about first), with its
  // session, if it named one, in the Nook panel.
  useEffect(() => {
    const asked = ws.takeRequest();
    if (!asked) return;
    const showSession = () => {
      if (asked.session == null || ws.folder == null) return;
      ws.rememberSession(ws.folder, asked.session);
      ws.setAssistantOpen(true);
    };
    if (ws.folder != null && samePath(ws.folder, asked.folder)) showSession();
    else withSaved(ws.dirtyTabs(), () => void ws.openFolder(asked.folder).then(showSession));
  });

  const choose = async (start: string | null) => {
    const picked = await chooseFolder(start).catch(() => null);
    if (picked) openFolder(picked);
  };

  /** The question the explorer's menu leads to, and what happens on its answer. */
  const askFor = (action: ExplorerAction): Ask => {
    switch (action.kind) {
      case "newFile":
        return {
          kind: "name",
          title: `New file in ${baseName(action.dir)}`,
          initial: "untitled.txt",
          confirm: "Create",
          onConfirm: (name) => ws.createFile(action.dir, name),
        };
      case "newFolder":
        return {
          kind: "name",
          title: `New folder in ${baseName(action.dir)}`,
          initial: "folder",
          confirm: "Create",
          onConfirm: (name) => ws.createFolder(action.dir, name),
        };
      case "rename":
        return {
          kind: "name",
          title: `Rename ${baseName(action.path)}`,
          initial: baseName(action.path),
          confirm: "Rename",
          onConfirm: (name) =>
            withSaved(
              ws.tabs.filter((t) => isUnder(t.path, action.path)),
              () => ws.rename(action.path, name),
            ),
        };
      case "delete":
        return {
          kind: "confirm",
          title: `Delete ${action.node.name}?`,
          body:
            (action.node.isDirectory ? "The folder and everything in it go for good; " : "It goes for good; ") +
            "Nook has no bin to bring it back from.",
          confirm: "Delete",
          onConfirm: () => ws.delete(action.node.path),
        };
    }
  };

  return (
    <PagePane>
      {ws.ready && (
        <>
          <Header ws={ws} repositories={repositories} onFolder={openFolder} onChoose={choose} />
          <HairLine />
          {ws.folder == null ? (
            <Welcome repositories={repositories} onFolder={openFolder} onChoose={() => void choose(repositories[0] ?? null)} />
          ) : (
            <div className="ide-body">
              {ws.explorerOpen && (
                <>
                  <Explorer ws={ws} width={ws.explorerWidth} onAction={(a) => setAsk(askFor(a))} />
                  <ResizeHandle
                    onDrag={(dx) => {
                      ws.explorerWidth = clampPanel(ws.explorerWidth + dx);
                      ws.changed();
                    }}
                    onDone={() => ws.persist()}
                  />
                </>
              )}
              <EditorPane ws={ws} onClose={(tab) => withSaved([tab], () => ws.close(tab))} />
              {ws.assistantOpen && (
                <>
                  <ResizeHandle
                    onDrag={(dx) => {
                      ws.assistantWidth = clampPanel(ws.assistantWidth - dx);
                      ws.changed();
                    }}
                    onDone={() => ws.persist()}
                  />
                  <AssistantPanel
                    ws={ws}
                    width={ws.assistantWidth}
                    onOpenModels={onOpenModels}
                    onClose={() => ws.setAssistantOpen(false)}
                  />
                </>
              )}
            </div>
          )}
          <HairLine />
          <StatusBar ws={ws} />
        </>
      )}
      {ask?.kind === "unsaved" && (
        <UnsavedDialog
          names={ask.tabs.map((t) => t.name)}
          onDismiss={() => setAsk(null)}
          onDiscard={() => {
            setAsk(null);
            ask.then();
          }}
          onSave={() => {
            setAsk(null);
            // The files are written before the folder changes, the tab closes or the file moves, and
            // a save that fails keeps the edits where they are.
            void Promise.all(ask.tabs.map((t) => ws.save(t))).then((saved) => {
              if (saved.every(Boolean)) ask.then();
            });
          }}
        />
      )}
      {ask?.kind === "name" && (
        <NameDialog
          title={ask.title}
          initial={ask.initial}
          confirm={ask.confirm}
          onDismiss={() => setAsk(null)}
          onConfirm={(name) => {
            setAsk(null);
            ask.onConfirm(name);
          }}
        />
      )}
      {ask?.kind === "confirm" && (
        <ConfirmDialog
          title={ask.title}
          body={ask.body}
          confirm={ask.confirm}
          onDismiss={() => setAsk(null)}
          onConfirm={() => {
            setAsk(null);
            ask.onConfirm();
          }}
        />
      )}
    </PagePane>
  );
}

// ====================================================================== header and status

function Header({
  ws,
  repositories,
  onFolder,
  onChoose,
}: {
  ws: IdeWorkspace;
  repositories: string[];
  onFolder: (path: string) => void;
  onChoose: (start: string | null) => void;
}) {
  const tab = ws.active;
  return (
    <div className="ide-header">
      <RepositoryPicker repository={ws.folder} locked={false} repositories={repositories} onRepository={onFolder} onChoose={onChoose} />
      {tab && (
        <>
          <span className="ide-crumbs caption">{ws.relativeName(tab).split("/").join("  ›  ")}</span>
          {tab.modified && <QuietAction text="Save" icon="check" onClick={() => void ws.save(tab)} />}
        </>
      )}
      <span className="ide-spacer" />
      <ToolbarIcon
        icon="layout-left"
        hint={ws.explorerOpen ? "Hide the files" : "Show the files"}
        onClick={() => ws.setExplorerOpen(!ws.explorerOpen)}
      />
      <ToolbarPill
        text="Nook"
        icon="sidebar-right"
        chevron={false}
        emphasised={ws.assistantOpen}
        onClick={() => ws.setAssistantOpen(!ws.assistantOpen)}
      />
    </div>
  );
}

function StatusBar({ ws }: { ws: IdeWorkspace }) {
  const folder = ws.folder;
  const tab = ws.active;
  return (
    <div className="ide-status caption">
      {folder && <span>{[baseName(folder), ws.branch].filter(Boolean).join("  ·  ")}</span>}
      <span className="ide-spacer" />
      {tab && (
        <>
          <span>
            Ln {tab.line}, Col {tab.column}
          </span>
          <span>{tab.language.tabs ? "Tabs" : "Spaces: 4"}</span>
          <span>{tab.charset}</span>
          <span>{tab.lineEnding}</span>
          <span>{tab.language.name}</span>
          {tab.conflict ? <span className="text-warning">Changed on disk</span> : tab.modified ? <span>Unsaved</span> : null}
        </>
      )}
    </div>
  );
}

// ====================================================================== no folder yet

function Welcome({
  repositories,
  onFolder,
  onChoose,
}: {
  repositories: string[];
  onFolder: (path: string) => void;
  onChoose: () => void;
}) {
  return (
    <div className="ide-welcome">
      <div className="ide-welcome__column">
        <Icon name="code" size={32} color="var(--text-secondary)" />
        <div className="h5">Open a folder to code in</div>
        <p className="body2">
          Browse and edit its files here, and ask Nook for changes beside them: the local worker writes in a private copy, you
          read the diff and apply it.
        </p>
        <div style={{ height: "var(--space-tiny)" }} />
        <Button text="Choose a folder…" onClick={onChoose} />
        {repositories.length > 0 && (
          <>
            <div className="overline ide-welcome__recent">Recent</div>
            {repositories.slice(0, 6).map((r) => (
              <button key={r} type="button" className="ide-recent-row" onClick={() => onFolder(r)}>
                <Icon name="folder-empty" size={18} color="var(--text-secondary)" />
                <span className="ide-repo-row__text">
                  <span className="body2 ide-repo-row__name ide-ellipsis">{repoName(r)}</span>
                  <span className="caption text-tertiary ide-ellipsis">{r}</span>
                </span>
              </button>
            ))}
          </>
        )}
      </div>
    </div>
  );
}
