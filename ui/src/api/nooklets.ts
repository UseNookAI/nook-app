/**
 * The Nooklets' finder (nook_core::nooklets, src-tauri/src/commands/nooklets.rs): the Nooklet for
 * a request typed in a sentence, in any language, and what the request sets for it ("into
 * German", "to PDF"). A small model on the processor reads the request (multilingual-e5-small,
 * one fixed download); until it is in, the request's words choose.
 *
 * Events on the topic "nooklets": `{ install }` while the finder downloads, null once done.
 */
import type { Install } from "./flows";
import { call, on } from "./ipc";

/** The Nooklets there are, by id. */
export type NookletId = "translate" | "transcribe" | "summarize" | "read-aloud" | "pdf" | "convert" | "screen";

/**
 * What a request sets for a Nooklet: the language to translate into, the spoken language to write
 * down, the language to summarize in or to read, or the format to convert to.
 */
export interface Preset {
  key: "language" | "format" | "mode";
  value: string;
  /** "German", "PDF". */
  label: string;
}

export interface Hit {
  id: NookletId;
  title: string;
  blurb: string;
  /** 0 to 1. */
  score: number;
  /** It may do what was asked. */
  fits: boolean;
  preset: Preset | null;
}

/**
 * Every Nooklet, the best first. `sure`: the first does what was asked; `matched`: it may;
 * `by`: "model", or "words" while the finder is not in.
 */
export interface Found {
  hits: Hit[];
  sure: boolean;
  matched: boolean;
  by: "model" | "words";
}

export interface FinderSetup {
  installed: boolean;
  /** What is still to download. */
  bytes: number;
  install: Install | null;
}

export const nookletsSetup = () => call<FinderSetup>("nooklets_setup");
export const nookletsFind = (request: string) => call<Found>("nooklets_find", { request });
/** Starts the finder's download; follow it with `onNooklets`. */
export const nookletsInstall = () => call<void>("nooklets_install");
export const nookletsCancelInstall = () => call<void>("nooklets_cancel_install");
export const nookletsClearInstallError = () => call<void>("nooklets_clear_install_error");
export const onNooklets = (fn: (e: { install?: Install | null }) => void) =>
  on<{ install?: Install | null }>("nooklets", (p) => fn(p ?? {}));
