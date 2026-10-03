/**
 * Which Nooklet is open, kept while the hub is rendered again (FlowMemory.flow), and the list of
 * Nooklets. Apart from the page itself, so the title strip (HubNav) and the sidebar can send the
 * page home or list the Nooklets without loading it.
 */
import type { NookletId } from "../../api/nooklets";

/** The Nooklets, in the sidebar's order. */
export type FlowKind = "TRANSLATE" | "TRANSCRIBE" | "SUMMARIZE" | "READ_ALOUD" | "PDF" | "CONVERT" | "SCREEN";

export interface NookletEntry {
  kind: FlowKind;
  id: NookletId;
  title: string;
  icon: string;
  blurb: string;
}

export const NOOKLETS: NookletEntry[] = [
  { kind: "TRANSLATE", id: "translate", title: "Translate speech", icon: "translate", blurb: "Speak or drop a file; hear it in another language" },
  { kind: "TRANSCRIBE", id: "transcribe", title: "Transcribe a recording", icon: "transcribe", blurb: "A meeting or voice memo, written down, with notes" },
  { kind: "SUMMARIZE", id: "summarize", title: "Summarize a document", icon: "summarize", blurb: "The key points of a long PDF, report or contract" },
  { kind: "READ_ALOUD", id: "read-aloud", title: "Read it aloud", icon: "read-aloud", blurb: "Any document or text, read by a natural voice" },
  { kind: "PDF", id: "pdf", title: "Edit a PDF", icon: "file-edit", blurb: "Change any text; the font stays the same" },
  { kind: "CONVERT", id: "convert", title: "Convert documents", icon: "file-convert", blurb: "Any document, sheet or picture into another format" },
  { kind: "SCREEN", id: "screen", title: "Record your screen", icon: "screen-record", blurb: "A screen, a window or an area, with sound; or stream it live" },
];

/** The Nooklet a flow run belongs to, by its `flow` (nook_core::flow::service's names). */
export function kindOfFlow(flow: string): FlowKind | null {
  switch (flow) {
    case "translate-audio":
      return "TRANSLATE";
    case "transcribe":
      return "TRANSCRIBE";
    case "summarize":
      return "SUMMARIZE";
    case "read-aloud":
      return "READ_ALOUD";
    default:
      return null;
  }
}

export const nookletOf = (kind: FlowKind): NookletEntry => NOOKLETS.find((n) => n.kind === kind) ?? NOOKLETS[0];

/** The open Nooklet, or the finder ("HOME"). */
export const FlowMemory = (() => {
  let flow: FlowKind | "HOME" = "HOME";
  const listeners = new Set<() => void>();
  return {
    get: () => flow,
    set(next: FlowKind | "HOME") {
      flow = next;
      listeners.forEach((l) => l());
    },
    subscribe(l: () => void) {
      listeners.add(l);
      return () => {
        listeners.delete(l);
      };
    },
  };
})();
