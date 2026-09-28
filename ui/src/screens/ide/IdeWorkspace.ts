/**
 * The Code page's state (IdeWorkspace.kt): the folder, the explorer, the open files and their
 * editors, the Nook panel's session, and what is remembered between starts (ide.json). One for
 * the app's life (see `ideWorkspace()`), so leaving the page and coming back finds everything as
 * it was. Components subscribe with `useWorkspace`; every change bumps a version and notifies.
 */
import { useSyncExternalStore } from "react";
import type { EditorView } from "@codemirror/view";
import type { EditorContext } from "../../api/code";
import { messageOf } from "../../api/ipc";
import {
  DEFAULT_PREFS,
  ideBranch,
  ideCreateFile,
  ideCreateFolder,
  ideDelete,
  ideFileTimes,
  ideListDir,
  ideLoadPrefs,
  ideReadFile,
  ideRename,
  ideResolveFolder,
  ideSavePrefs,
  ideWriteFile,
  MAX_PANEL,
  MIN_PANEL,
  type FileNode,
  type IdePrefs,
} from "../../api/ide";
import { baseName } from "../../components/paths";
import { registerLeaveCheck } from "../../shell/unsaved";
import { createEditor, type EditorHandle } from "./EditorHost";
import {
  isUnder,
  languageFor,
  nameProblem,
  relativeName,
  selectedLines,
  selectedText,
  type Language,
} from "./IdeSupport";

/** One open file: its editor, how it was read (line ending, charset), and what the page shows about it. */
export class EditorTab {
  readonly key: string;
  readonly name: string;
  modified = false;
  line = 1;
  column = 1;
  /** The file changed on disk while this tab holds edits of its own; saving overwrites it. */
  conflict = false;

  constructor(
    readonly path: string,
    readonly editor: EditorHandle,
    readonly language: Language,
    readonly lineEnding: string,
    readonly charset: string,
    public diskTime: number | null,
  ) {
    this.key = path;
    this.name = baseName(path);
  }

  get view(): EditorView {
    return this.editor.view;
  }
}

export const clampPanel = (px: number) => Math.min(MAX_PANEL, Math.max(MIN_PANEL, px));

export class IdeWorkspace {
  /** False until ide.json has been read. */
  ready = false;
  folder: string | null = null;
  tabs: EditorTab[] = [];
  active: EditorTab | null = null;
  expanded = new Set<string>();
  children = new Map<string, FileNode[]>();
  /** The explorer's highlighted entry: the last one clicked, which new files go beside. */
  selected: string | null = null;
  selectedIsDir = false;
  explorerWidth = DEFAULT_PREFS.explorerWidth;
  assistantWidth = DEFAULT_PREFS.assistantWidth;
  explorerOpen = DEFAULT_PREFS.explorerOpen;
  assistantOpen = DEFAULT_PREFS.assistantOpen;
  findOpen = false;
  /** Bumped by Ctrl+F so an open find bar takes the focus again. */
  findFocus = 0;
  /** Whether a request to Nook says which file is open in the editor, and what is selected in it. */
  shareOpenFile = true;
  branch: string | null = null;
  /** What went wrong, for snackbars; the page shows them and clears them. */
  notices: string[] = [];
  /** The Swing CardLayout panel's stand-in: one card per open file, the active one shown. */
  readonly host: HTMLDivElement;

  private sessions: Record<string, string> = {};
  private version = 0;
  private listeners = new Set<() => void>();
  private pendingPrefs: IdePrefs | null = null;
  private saving: Promise<void> = Promise.resolve();
  private checking = false;

  constructor() {
    this.host = document.createElement("div");
    this.host.className = "ide-editor-host";
    void this.init();
  }

  // ------------------------------------------------------------------ the store

  subscribe = (fn: () => void) => {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  };

  getVersion = () => this.version;

  /** Tells the page something changed. */
  changed() {
    this.version++;
    this.listeners.forEach((fn) => fn());
  }

  private say(message: string) {
    this.notices = [...this.notices, message];
    this.changed();
  }

  takeNotices(): string[] {
    const n = this.notices;
    this.notices = [];
    return n;
  }

  private async init() {
    let prefs: IdePrefs;
    try {
      prefs = { ...DEFAULT_PREFS, ...(await ideLoadPrefs()) };
    } catch {
      prefs = { ...DEFAULT_PREFS };
    }
    this.folder = prefs.folder;
    this.explorerWidth = clampPanel(prefs.explorerWidth);
    this.assistantWidth = clampPanel(prefs.assistantWidth);
    this.explorerOpen = prefs.explorerOpen;
    this.assistantOpen = prefs.assistantOpen;
    this.sessions = { ...prefs.sessions };
    this.ready = true;
    this.changed();
    const dir = this.folder;
    if (dir) {
      void this.list(dir);
      void this.readBranch(dir);
    }
    // The files open last time open again, and the one in front comes to the front; not without
    // their folder (it is gone), or they would sit behind the welcome page.
    if (dir && prefs.open.length > 0) {
      let times: (number | null)[] = [];
      try {
        times = await ideFileTimes(prefs.open);
      } catch {
        // Nothing to reopen.
      }
      for (let i = 0; i < prefs.open.length; i++) {
        if (times[i] != null) await this.load(prefs.open[i]);
      }
      const last = this.tabs.find((t) => t.key === prefs.active);
      if (last) this.activate(last);
    }
  }

  // ------------------------------------------------------------------ the folder and the explorer

  /** Opens `dir` in the explorer, closing the last folder's files; the page asks about unsaved edits first. */
  async openFolder(dir: string) {
    let target: string;
    try {
      target = await ideResolveFolder(dir);
    } catch {
      target = dir;
    }
    if (target === this.folder) return;
    this.closeAll();
    this.folder = target;
    this.expanded = new Set();
    this.children = new Map();
    this.selected = null;
    this.selectedIsDir = false;
    this.branch = null;
    this.changed();
    void this.list(target);
    void this.readBranch(target);
    this.persist();
  }

  private async readBranch(dir: string) {
    try {
      const branch = await ideBranch(dir);
      if (this.folder === dir) {
        this.branch = branch;
        this.changed();
      }
    } catch {
      // No branch to show.
    }
  }

  private async list(dir: string) {
    let entries: FileNode[] = [];
    try {
      entries = await ideListDir(dir);
    } catch {
      // A folder that cannot be read lists as empty.
    }
    this.children = new Map(this.children).set(dir, entries);
    this.changed();
  }

  /** Opens or closes a folder in the explorer, listing it the first time. */
  toggle(dir: string) {
    const next = new Set(this.expanded);
    if (next.has(dir)) {
      next.delete(dir);
    } else {
      next.add(dir);
      if (!this.children.has(dir)) void this.list(dir);
    }
    this.expanded = next;
    this.changed();
  }

  select(path: string, isDir: boolean) {
    this.selected = path;
    this.selectedIsDir = isDir;
    this.changed();
  }

  /** Lists the folder and every open folder again: after a save, an Apply, or a change made elsewhere. */
  async refresh() {
    const root = this.folder;
    if (!root) return;
    const dirs = [root, ...[...this.expanded].filter((d) => isUnder(d, root))];
    const lists = await Promise.all(dirs.map((d) => ideListDir(d).catch(() => [] as FileNode[])));
    if (this.folder !== root) return;
    this.children = new Map(dirs.map((d, i) => [d, lists[i]]));
    this.changed();
  }

  // ------------------------------------------------------------------ files in the editor

  /** Shows `file` in the editor, reading it if it is not open yet. */
  async open(file: string) {
    this.selected = file;
    this.selectedIsDir = false;
    const tab = this.tabs.find((t) => t.path === file);
    if (tab) {
      this.activate(tab);
      return;
    }
    this.changed();
    const loaded = await this.load(file);
    if (loaded) this.activate(loaded);
  }

  /** Reads `file` into a new tab, or says why it cannot; null then. */
  private async load(file: string): Promise<EditorTab | null> {
    let loaded;
    try {
      loaded = await ideReadFile(file);
    } catch (e) {
      this.say(messageOf(e) || `Could not open ${baseName(file)}.`);
      return null;
    }
    const already = this.tabs.find((t) => t.path === file);
    if (already) return already;
    const language = languageFor(baseName(file));
    let tab: EditorTab | null = null;
    const editor = createEditor(this.host, loaded.text, language, {
      changed: () => {
        if (tab && !tab.modified) {
          tab.modified = true;
          this.changed();
        }
      },
      caret: (line, column) => {
        if (!tab) return;
        tab.line = line;
        tab.column = column;
        this.changed();
      },
      save: () => {
        if (tab) void this.save(tab);
      },
      find: () => {
        this.findOpen = true;
        this.findFocus++;
        this.changed();
      },
    });
    editor.card.style.display = "none";
    tab = new EditorTab(file, editor, language, loaded.lineEnding, loaded.charset, loaded.time);
    this.tabs = [...this.tabs, tab];
    this.changed();
    this.persist();
    return tab;
  }

  activate(tab: EditorTab | null) {
    this.active = tab;
    for (const t of this.tabs) t.editor.card.style.display = t === tab ? "" : "none";
    if (tab) {
      this.selected = tab.path;
      this.selectedIsDir = false;
      tab.view.requestMeasure();
      requestAnimationFrame(() => {
        if (this.active === tab) tab.view.focus();
      });
    }
    this.changed();
    this.persist();
  }

  /** Closes a tab, edits and all; the page asks about unsaved edits first. */
  close(tab: EditorTab) {
    const i = this.tabs.indexOf(tab);
    if (i < 0) return;
    this.tabs = this.tabs.filter((t) => t !== tab);
    tab.view.destroy();
    tab.editor.card.remove();
    if (this.active === tab) this.activate(this.tabs[Math.min(i, this.tabs.length - 1)] ?? null);
    this.changed();
    this.persist();
  }

  closeAll() {
    for (const t of this.tabs) {
      t.view.destroy();
      t.editor.card.remove();
    }
    this.tabs = [];
    this.active = null;
    this.findOpen = false;
    this.changed();
  }

  dirtyTabs(): EditorTab[] {
    return this.tabs.filter((t) => t.modified);
  }

  /** Writes the tab's text back the way the file was read: its line ending and charset. True when it was written. */
  async save(tab: EditorTab): Promise<boolean> {
    const doc = tab.view.state.doc;
    try {
      const time = await ideWriteFile(tab.path, doc.toString(), tab.lineEnding, tab.charset);
      tab.diskTime = time;
      // Typing that went on while the file was written is still unsaved.
      tab.modified = tab.view.state.doc !== doc;
      tab.conflict = false;
      this.changed();
      return true;
    } catch (e) {
      this.say(`Could not save ${tab.name}: ${messageOf(e)}`);
      return false;
    }
  }

  /**
   * Files changed outside the editor (an Apply from the Nook panel, another program): a tab with
   * no edits of its own reads the file again; one with edits keeps them and is marked.
   */
  async checkDisk() {
    if (this.checking || this.tabs.length === 0) return;
    this.checking = true;
    try {
      const tabs = [...this.tabs];
      const times = await ideFileTimes(tabs.map((t) => t.path)).catch(() => [] as (number | null)[]);
      for (let i = 0; i < tabs.length; i++) {
        const tab = tabs[i];
        const now = times[i];
        if (now == null || now === tab.diskTime || !this.tabs.includes(tab)) continue;
        if (tab.modified) {
          if (!tab.conflict) this.say(`${tab.name} changed on disk while it has unsaved edits here. Saving keeps yours.`);
          tab.conflict = true;
          tab.diskTime = now;
          this.changed();
          continue;
        }
        let loaded;
        try {
          loaded = await ideReadFile(tab.path);
        } catch {
          continue;
        }
        if (!this.tabs.includes(tab) || tab.modified) continue;
        tab.editor.replaceText(loaded.text);
        tab.modified = false;
        tab.diskTime = loaded.time;
        this.changed();
      }
    } finally {
      this.checking = false;
    }
  }

  // ------------------------------------------------------------------ the explorer's changes

  createFile(dir: string, name: string) {
    void this.change(`create ${name.trim()}`, async () => ideCreateFile(dir, name));
  }

  createFolder(dir: string, name: string) {
    void this.change(`create ${name.trim()}`, async () => {
      await ideCreateFolder(dir, name);
      this.expanded = new Set(this.expanded).add(dir);
      return null;
    });
  }

  rename(path: string, name: string) {
    // A name that cannot work is refused before its open files close.
    const problem = nameProblem(name);
    if (problem) {
      this.say(`Could not rename ${baseName(path)}: ${problem}`);
      return;
    }
    for (const t of this.tabs.filter((t) => isUnder(t.path, path))) this.close(t);
    void this.change(`rename ${baseName(path)}`, async () => {
      const node = await ideRename(path, name);
      return node.isDirectory ? null : node.path;
    });
  }

  /** Removes a file, or a folder with everything in it. */
  delete(path: string) {
    for (const t of this.tabs.filter((t) => isUnder(t.path, path))) this.close(t);
    void this.change(`delete ${baseName(path)}`, async () => {
      await ideDelete(path);
      return null;
    });
  }

  /** Runs a change to the files, lists the folder again and opens the file it returns. */
  private async change(what: string, block: () => Promise<string | null>) {
    let opened: string | null = null;
    try {
      opened = await block();
    } catch (e) {
      this.say(`Could not ${what}: ${messageOf(e)}`);
    }
    await this.refresh();
    if (opened) void this.open(opened);
  }

  // ------------------------------------------------------------------ the Nook panel

  sessionFor(dir: string): string | null {
    return this.sessions[dir] ?? null;
  }

  rememberSession(dir: string, id: string) {
    this.sessions = { ...this.sessions, [dir]: id };
    this.persist();
  }

  /** The active file's path within the folder, with the folder's slashes made plain. */
  relativeName(tab: EditorTab): string {
    return relativeName(this.folder, tab.path);
  }

  /** The selected lines of the active file, first and last, or null when nothing is selected. */
  selectedLines(): [number, number] | null {
    return this.active ? selectedLines(this.active.view.state) : null;
  }

  /**
   * What a request carries from the editor (the original's `decorate`): the open file and, when
   * there is one, the selection with its lines. Null when the person turned it off or no file is open.
   */
  editorContext(): EditorContext | null {
    const tab = this.active;
    if (!this.shareOpenFile || !tab) return null;
    const lines = selectedLines(tab.view.state);
    const selection = selectedText(tab.view.state);
    return {
      file: this.relativeName(tab),
      selection: lines && selection ? selection : null,
      lines: lines && selection ? lines : null,
    };
  }

  setShareOpenFile(on: boolean) {
    this.shareOpenFile = on;
    this.changed();
  }

  // ------------------------------------------------------------------ panels

  setExplorerOpen(open: boolean) {
    this.explorerOpen = open;
    this.changed();
    this.persist();
  }

  setAssistantOpen(open: boolean) {
    this.assistantOpen = open;
    this.changed();
    this.persist();
  }

  setFindOpen(open: boolean) {
    this.findOpen = open;
    this.changed();
  }

  // ------------------------------------------------------------------ plumbing

  /**
   * Remembers the page's state in ide.json. One save at a time: two at once raced on the same
   * temporary file in the original (2026-09-25). A save asked for while one is running waits for
   * it and writes the newest state.
   */
  persist() {
    if (!this.ready) return;
    const queued = this.pendingPrefs !== null;
    this.pendingPrefs = {
      folder: this.folder,
      open: this.tabs.map((t) => t.key),
      active: this.active?.key ?? null,
      explorerWidth: Math.round(this.explorerWidth),
      assistantWidth: Math.round(this.assistantWidth),
      explorerOpen: this.explorerOpen,
      assistantOpen: this.assistantOpen,
      sessions: this.sessions,
    };
    if (queued) return;
    this.saving = this.saving.then(async () => {
      const prefs = this.pendingPrefs;
      this.pendingPrefs = null;
      if (!prefs) return;
      try {
        await ideSavePrefs(prefs);
      } catch (e) {
        console.debug("Could not save the Code page's state:", messageOf(e));
      }
    });
  }
}

let memory: IdeWorkspace | null = null;

// Closing the window asks about edited files, from whatever page is open.
registerLeaveCheck("code", () => (memory ? memory.dirtyTabs().map((t) => `${t.name} has changes that are not saved yet`) : []));

/** The one workspace, made on the first visit to the Code page and kept for the app's life (IdeMemory). */
export function ideWorkspace(): IdeWorkspace {
  memory ??= new IdeWorkspace();
  return memory;
}

/** Re-renders the caller whenever the workspace changes. */
export function useWorkspace(ws: IdeWorkspace): number {
  return useSyncExternalStore(ws.subscribe, ws.getVersion);
}
