/**
 * Which Nooklet is open, kept while the hub is rendered again (FlowMemory.flow). Apart from the
 * page itself, so the title strip (HubNav) can send the page home without loading it.
 */

/** The Nooklets, in the rail's order. */
export type FlowKind = "TRANSLATE" | "TRANSCRIBE" | "SUMMARIZE" | "READ_ALOUD" | "PDF" | "CONVERT" | "SCREEN";

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
