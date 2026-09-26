/**
 * The document converter Nooklet (nook_core::convert, src-tauri/src/commands/convert.rs). Types
 * mirror the Rust of `ConvertService` (Offer, Target, Need, Job, Item) field for field.
 *
 * Office files go through an office suite, so they keep their layout: Word, Excel or PowerPoint
 * when they are on the computer, else LibreOffice. Text documents go through Pandoc, web pages to
 * PDF through Edge, PDFs through PDFium, pictures and tables through Nook itself. Pandoc, PDFium
 * and LibreOffice are downloaded when a conversion first needs them.
 *
 * Events on the topic "convert": `{ job }` when a job is added or moves on, `{ install }` while
 * an engine downloads (null once done).
 */
import type { Install } from "./flows";
import { call, inTauri, on } from "./ipc";

/** What a format holds; the picker groups targets by it. */
export type Kind = "DOCUMENT" | "PDF" | "WEB" | "TEXT" | "EBOOK" | "SHEET" | "SLIDES" | "IMAGE";

/** What does a conversion's work: the three downloaded engines, and the computer's own Edge and Office. */
export type Engine = "PANDOC" | "PDF" | "EDGE" | "MS_OFFICE" | "LIBRE_OFFICE";

/** A chosen file as the converter sees it: its format, when Nook reads it. */
export interface FileInfo {
  path: string;
  name: string;
  format: string | null;
  formatName: string | null;
  kind: Kind | null;
}

/** An engine still to download for a target. */
export interface Need {
  engine: Engine;
  /** "the document engine (Pandoc)". */
  what: string;
  bytes: number;
}

/** A format the files can become. */
export interface Target {
  /** "docx". */
  id: string;
  /** "Word document". */
  name: string;
  kind: Kind;
  /** Who does the work: "Word", "Pandoc + Edge". */
  by: string;
  needs: Need[];
  /** Why it cannot be done on this computer. */
  missing: string | null;
}

export interface Offer {
  files: FileInfo[];
  targets: Target[];
  /** The files are pictures, and can go into one PDF. */
  combine: boolean;
  /** Why there is nothing to offer, when there is not. */
  note: string | null;
}

export type Status = "WAITING" | "CONVERTING" | "DONE" | "FAILED" | "STOPPED";

/** One file of a job, and what became of it. */
export interface Item {
  input: string;
  name: string;
  status: Status;
  /** The files written: one, or several (a workbook's sheets, a PDF's pages as pictures). */
  outputs: string[];
  error: string | null;
}

export interface Job {
  id: string;
  to: string;
  toName: string;
  /** When it was asked for, ms since the epoch. */
  at: number;
  status: Status;
  items: Item[];
}

export type ConvertEvent = { job?: Job; install?: Install | null };

/** The extensions Nook reads (formats.rs FORMATS, with the other names each goes by). */
export const READS = [
  "pdf", "docx", "docm", "dotx", "doc", "dot", "odt", "rtf", "html", "htm", "xhtml", "md", "markdown", "txt", "text",
  "tex", "latex", "rst", "org", "adoc", "asciidoc", "typ", "wiki", "mediawiki", "ipynb", "epub", "fb2", "xlsx", "xlsm",
  "xltx", "xls", "xlt", "ods", "csv", "tsv", "tab", "json", "pptx", "pptm", "ppsx", "ppt", "pps", "odp", "png", "jpg",
  "jpeg", "jfif", "jpe", "webp", "bmp", "dib", "tiff", "tif", "gif", "ico",
];

export const convertOffer = (paths: string[]) => call<Offer>("convert_offer", { paths });
/** Starts the download of `engines` (those not in yet); follow it with `onConvert`. */
export const convertInstall = (engines: Engine[]) => call<void>("convert_install", { engines });
export const convertInstallState = () => call<Install | null>("convert_install_state");
export const convertCancelInstall = () => call<void>("convert_cancel_install");
export const convertClearInstallError = () => call<void>("convert_clear_install_error");
/** Converts `paths` into `to`: beside each file, or into `folder`; pictures into one PDF when `combine`. */
export const convertStart = (paths: string[], to: string, combine: boolean, folder: string | null) =>
  call<Job>("convert_start", { paths, to, combine, folder });
/** The jobs so far, newest first. */
export const convertJobs = () => call<Job[]>("convert_jobs");
export const convertCancel = (id: string) => call<void>("convert_cancel", { id });
/** Opens a result in the program Windows opens it with. */
export const convertOpen = (path: string) => call<void>("convert_open", { path });
/** Shows a result selected in Explorer. */
export const convertReveal = (path: string) => call<void>("convert_reveal", { path });
export const onConvert = (fn: (e: ConvertEvent) => void) => on<ConvertEvent>("convert", (p) => fn(p ?? {}));

/** Windows' Open dialog for the files to convert; empty when nothing was chosen. */
export async function chooseDocuments(): Promise<string[]> {
  if (inTauri) {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({
      title: "Choose files to convert",
      multiple: true,
      directory: false,
      filters: [
        { name: "Documents, sheets, slides and pictures", extensions: READS },
        { name: "All files", extensions: ["*"] },
      ],
    });
    return Array.isArray(picked) ? picked : typeof picked === "string" ? [picked] : [];
  }
  const typed = window.prompt("Choose files to convert (separate them with |)", "C:\\Users\\you\\Documents\\report.docx");
  return typed
    ? typed
        .split("|")
        .map((s) => s.trim())
        .filter(Boolean)
    : [];
}

/** Windows' folder picker for where the results go; null when cancelled. */
export async function chooseOutputFolder(): Promise<string | null> {
  if (inTauri) {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({ title: "Where the converted files go", directory: true, multiple: false });
    return typeof picked === "string" ? picked : null;
  }
  const typed = window.prompt("Where the converted files go", "C:\\Users\\you\\Documents\\Converted");
  return typed && typed.trim() ? typed.trim() : null;
}
