/**
 * The PDF editor flow (nook_core::pdf, src-tauri/src/commands/pdf.rs). Types mirror the Rust of
 * `PdfEditor` (PdfDoc, PdfPageInfo, Pick, Block, BlockLine, BlockFont, Replaced) field for field.
 * Coordinates are PDF points on the page, y going up from the bottom edge.
 *
 * Text that is part of a picture (a scan, a form printed flat) is read by Windows' own text
 * recognition: its Block carries `drawn`, and the new text goes over the old letters as text in
 * the matched face, or (`drawn.fromPage`) made of letters cut from the page itself.
 */
import type { Install } from "./flows";
import { call, inTauri, on } from "./ipc";

export interface Rect {
  left: number;
  bottom: number;
  right: number;
  top: number;
}

export interface PdfPageInfo {
  /** In points (1/72 inch). */
  width: number;
  height: number;
  /** Bumped by every edit to the page: draw it again when it changes. */
  version: number;
}

export interface PdfDoc {
  id: string;
  name: string;
  path: string;
  pages: PdfPageInfo[];
  edits: number;
  canUndo: boolean;
  /** Some edits are not saved yet. */
  dirty: boolean;
  savedTo: string | null;
}

/** Which edge of a line stays put when its length changes. */
export type Align = "LEFT" | "RIGHT" | "CENTER";

/** Where the person pointed: a dragged box, or a click. */
export type Area = { kind: "rect"; rect: Rect } | { kind: "point"; x: number; y: number };

/** How the picked text looks, for the editing box. */
export interface BlockFont {
  name: string;
  family: string;
  bold: boolean;
  italic: boolean;
  serif: boolean;
  mono: boolean;
  /** "#1f4e79". */
  color: string;
}

export interface BlockLine {
  objects: number[];
  text: string;
  rect: Rect;
  baseline: number;
  /** The font size on the page, in points. */
  size: number;
}

/** Text that is part of a picture: the paper's colour, laid over the old letters, and the installed face they matched. */
export interface Drawn {
  /** "#fbfaf6". */
  paper: string;
  /** The face's key among the installed fonts ("timesnewromanbold"). */
  face: string;
  /** How much wider the letters are than the face at their size. */
  stretch: number;
  /** Set by the person: the new text is made of letters cut from the page itself. */
  fromPage?: boolean;
}

/** Picked text, handed back with the new text to `pdfReplace`. */
export interface Block {
  page: number;
  version: number;
  lines: BlockLine[];
  rect: Rect;
  font: BlockFont;
  /** The edge kept by default: the right one for figures. */
  align: Align;
  /** Set when the text is part of a picture, not text objects. */
  drawn?: Drawn | null;
}

export interface Pick {
  block: Block | null;
  /** Why nothing was picked. */
  why: string | null;
}

export interface Replaced {
  doc: PdfDoc;
  /** What the person should know: a font from Windows, text past the page's edge. */
  note: string | null;
}

/** Whether the PDF engine is in, its download's size, and the download while it runs. */
export interface PdfSetup {
  installed: boolean;
  bytes: number;
  install: Install | null;
}

export const pdfSetup = () => call<PdfSetup>("pdf_setup");
/** Starts the PDF engine's download (about 4 MB); follow it with `onPdf`. */
export const pdfInstall = () => call<void>("pdf_install");
export const pdfCancelInstall = () => call<void>("pdf_cancel_install");
export const pdfClearInstallError = () => call<void>("pdf_clear_install_error");
/** The PDF engine's download: `{ install }`, null once it is done or forgotten. */
export const onPdf = (fn: (e: { install?: Install | null }) => void) => on<{ install?: Install | null }>("pdf", (p) => fn(p ?? {}));
export const pdfOpen = (path: string) => call<PdfDoc>("pdf_open", { path });
export const pdfDocs = () => call<PdfDoc[]>("pdf_docs");
export const pdfPick = (id: string, page: number, area: Area) => call<Pick>("pdf_pick", { id, page, area });
export const pdfReplace = (id: string, block: Block, texts: string[], align: Align) =>
  call<Replaced>("pdf_replace", { id, block, texts, align });
export const pdfUndo = (id: string) => call<PdfDoc>("pdf_undo", { id });
/** Saves to `path`, or beside the original as "<name> (edited).pdf" for null. */
export const pdfSave = (id: string, path: string | null) => call<PdfDoc>("pdf_save", { id, path });
export const pdfClose = (id: string) => call<void>("pdf_close", { id });
export const pdfReveal = (path: string) => call<void>("pdf_reveal", { path });

/** A page drawn `width` pixels wide, as an object URL for an <img> (revoke it when done). */
export async function pdfRender(id: string, page: number, width: number): Promise<string> {
  const bytes = await call<ArrayBuffer | number[]>("pdf_render", { id, page, width: Math.round(width) });
  const data = bytes instanceof ArrayBuffer ? bytes : new Uint8Array(bytes).buffer;
  return URL.createObjectURL(new Blob([data], { type: "image/png" }));
}

/** Windows' Open dialog for a PDF; null when nothing was chosen. */
export async function choosePdf(): Promise<string | null> {
  if (inTauri) {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({
      title: "Choose a PDF to edit",
      multiple: false,
      directory: false,
      filters: [{ name: "PDF", extensions: ["pdf"] }],
    });
    return typeof picked === "string" ? picked : null;
  }
  const typed = window.prompt("Choose a PDF to edit", "C:\\Users\\you\\Documents\\invoice.pdf");
  return typed && typed.trim() ? typed.trim() : null;
}

/** Windows' Save dialog for where the edited PDF goes; null when cancelled. */
export async function chooseSavePath(suggested: string): Promise<string | null> {
  if (inTauri) {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const picked = await save({ title: "Save the edited PDF", defaultPath: suggested, filters: [{ name: "PDF", extensions: ["pdf"] }] });
    return typeof picked === "string" ? picked : null;
  }
  const typed = window.prompt("Save the edited PDF as", suggested);
  return typed && typed.trim() ? typed.trim() : null;
}
