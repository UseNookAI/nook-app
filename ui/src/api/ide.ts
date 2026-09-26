/**
 * The Code page's disk side (nook_core::ide, src-tauri/src/commands/ide.rs): the folder tree, the
 * files the editor opens and saves, the explorer's changes, and `<home>\code\ide.json`. Types
 * mirror the Rust structs (FileNode, LoadedFile, IdePrefs) field for field.
 */
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { call, inTauri } from "./ipc";

/** One entry of a folder, for the explorer. */
export interface FileNode {
  path: string;
  name: string;
  isDirectory: boolean;
}

/** A file as the editor holds it: text with "\n" breaks, how it was read, and when it last changed. */
export interface LoadedFile {
  text: string;
  /** "CRLF" or "LF"; a save writes it back. */
  lineEnding: string;
  /** "UTF-8" or "ISO-8859-1"; a save writes it back. */
  charset: string;
  /** Modification time, ms since the epoch. */
  time: number;
}

/** What the Code page keeps between starts (IdePrefs.kt). */
export interface IdePrefs {
  folder: string | null;
  open: string[];
  active: string | null;
  explorerWidth: number;
  assistantWidth: number;
  explorerOpen: boolean;
  assistantOpen: boolean;
  /** The Nook session of each folder, by the folder's path. */
  sessions: Record<string, string>;
}

/** What a side panel may be dragged to, in px. */
export const MIN_PANEL = 180;
export const MAX_PANEL = 720;

export const DEFAULT_PREFS: IdePrefs = {
  folder: null,
  open: [],
  active: null,
  explorerWidth: 240,
  assistantWidth: 400,
  explorerOpen: true,
  assistantOpen: true,
  sessions: {},
};

/** The prefs remembered last time; a folder that is gone comes back as null. */
export const ideLoadPrefs = () => call<IdePrefs>("ide_load_prefs");
export const ideSavePrefs = (prefs: IdePrefs) => call<void>("ide_save_prefs", { prefs });
/** The folder as the page keys it: absolute, normalized. */
export const ideResolveFolder = (path: string) => call<string>("ide_resolve_folder", { path });
/** The git branch checked out in the folder, or null. */
export const ideBranch = (dir: string) => call<string | null>("ide_branch", { dir });
/** Folders first, by name; ".git" left out; an unreadable folder lists as empty. */
export const ideListDir = (dir: string) => call<FileNode[]>("ide_list_dir", { dir });
/** Rejects with "x is not a text file." or "x is N MB, too big for the editor." */
export const ideReadFile = (path: string) => call<LoadedFile>("ide_read_file", { path });
/** Saves with the file's line ending and charset; resolves to the new modification time. */
export const ideWriteFile = (path: string, text: string, lineEnding: string, charset: string) =>
  call<number>("ide_write_file", { path, text, lineEnding, charset });
/** The modification time of each path that is a file, null for the rest. */
export const ideFileTimes = (paths: string[]) => call<(number | null)[]>("ide_file_times", { paths });
export const ideCreateFile = (dir: string, name: string) => call<string>("ide_create_file", { dir, name });
export const ideCreateFolder = (dir: string, name: string) => call<string>("ide_create_folder", { dir, name });
export const ideRename = (path: string, name: string) => call<FileNode>("ide_rename", { path, name });
/** Deletes a file, or a folder with everything in it, for good. */
export const ideDelete = (path: string) => call<void>("ide_delete", { path });

/**
 * Windows' folder picker (it replaces the original's nook-folder.exe). Null when cancelled. In a
 * plain browser the mocks pick their demo folder.
 */
export async function chooseFolder(start: string | null): Promise<string | null> {
  const title = "Choose the folder Nook should work on";
  if (!inTauri) return call<string | null>("ide_mock_choose_folder", { start });
  const picked = await openDialog({ directory: true, multiple: false, defaultPath: start ?? undefined, title });
  return typeof picked === "string" ? picked : null;
}
