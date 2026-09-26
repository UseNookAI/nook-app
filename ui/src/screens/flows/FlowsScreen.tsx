/**
 * FlowsScreen.kt, now the Nooklets page: ready-made jobs, done in steps on this computer. It opens
 * on the finder (NookletsHome), which asks what the person wants done; the Nooklet it opens sits
 * on the right, with the Nooklets in a rail on the left and the way back to the finder at its top.
 */
import { useState, useSyncExternalStore } from "react";
import { Icon } from "../../components/Icon";
import type { NookletId, Preset } from "../../api/nooklets";
import { PagePane } from "../hub/PagePane";
import { ConvertFlow } from "./ConvertFlow";
import { NookletsHome } from "./NookletsHome";
import { PdfFlow } from "./PdfFlow";
import { TranslateFlow } from "./TranslateFlow";
import "./flows.css";
import "./nooklets.css";

/** The Nooklets, in the rail's order. */
export type FlowKind = "TRANSLATE" | "PDF" | "CONVERT";

const FLOWS: { kind: FlowKind; id: NookletId; title: string; icon: string; blurb: string }[] = [
  {
    kind: "TRANSLATE",
    id: "translate",
    title: "Translate speech",
    icon: "translate",
    blurb: "Speak or drop a file; hear it in another language",
  },
  {
    kind: "PDF",
    id: "pdf",
    title: "Edit a PDF",
    icon: "file-edit",
    blurb: "Change any text; the font stays the same",
  },
  {
    kind: "CONVERT",
    id: "convert",
    title: "Convert documents",
    icon: "file-convert",
    blurb: "Any document, sheet or picture into another format",
  },
];

/** The open Nooklet, or the finder ("HOME"), kept while the hub is rendered again (FlowMemory.flow). */
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

/** What the finder's request set, for the Nooklet it opened; `nonce` tells one request from the next. */
interface Asked {
  kind: FlowKind;
  preset: Preset;
  nonce: number;
}

let nonce = 0;

export function FlowsScreen({ say, onOpenModels }: { say: (message: string) => void; onOpenModels: () => void }) {
  const flow = useSyncExternalStore(FlowMemory.subscribe, FlowMemory.get);
  const [asked, setAsked] = useState<Asked | null>(null);
  // An open PDF wants the room: the rail folds to its icons.
  const [narrow, setNarrow] = useState(false);

  const open = (id: NookletId, preset: Preset | null) => {
    const kind = FLOWS.find((f) => f.id === id)?.kind ?? "TRANSLATE";
    setAsked(preset ? { kind, preset, nonce: ++nonce } : null);
    FlowMemory.set(kind);
  };

  if (flow === "HOME") {
    return (
      <PagePane>
        <div className="fl-page">
          <div className="fl-main">
            <NookletsHome say={say} onOpen={open} />
          </div>
        </div>
      </PagePane>
    );
  }

  const presetFor = (kind: FlowKind) => (asked?.kind === kind ? asked : null);
  const language = presetFor("TRANSLATE");
  const format = presetFor("CONVERT");
  return (
    <PagePane>
      <div className="fl-page">
        <nav className={narrow && flow === "PDF" ? "fl-rail fl-rail--narrow" : "fl-rail"}>
          <div className="h6 fl-rail__title-text">Nooklets</div>
          <div className="caption text-tertiary fl-rail__blurb">Ready-made jobs that run on your own computer, step by step.</div>
          <div className="fl-rail__list">
            <button type="button" className="fl-rail__item fl-rail__find" title="Ask for a Nooklet" onClick={() => FlowMemory.set("HOME")}>
              <span className="fl-rail__icon">
                <Icon name="search" size={16} />
              </span>
              <span className="fl-rail__text">
                <span className="body2 fl-rail__title">Ask for a Nooklet</span>
                <span className="caption text-tertiary">Say what you want done</span>
              </span>
            </button>
            {FLOWS.map((f) => (
              <button
                key={f.kind}
                type="button"
                className={f.kind === flow ? "fl-rail__item fl-rail__item--selected" : "fl-rail__item"}
                aria-current={f.kind === flow ? "page" : undefined}
                title={f.title}
                onClick={() => FlowMemory.set(f.kind)}
              >
                <span className="fl-rail__icon">
                  <Icon name={f.icon} size={16} />
                </span>
                <span className="fl-rail__text">
                  <span className="body2 fl-rail__title">{f.title}</span>
                  <span className="caption text-tertiary">{f.blurb}</span>
                </span>
              </button>
            ))}
          </div>
        </nav>
        <div className="fl-main">
          {flow === "TRANSLATE" && (
            <TranslateFlow
              say={say}
              onOpenModels={onOpenModels}
              preset={language ? { language: language.preset.value, nonce: language.nonce } : null}
            />
          )}
          {flow === "PDF" && <PdfFlow say={say} onOpenChange={setNarrow} />}
          {flow === "CONVERT" && (
            <ConvertFlow
              say={say}
              preset={format ? { format: format.preset.value, label: format.preset.label, nonce: format.nonce } : null}
            />
          )}
        </div>
      </div>
    </PagePane>
  );
}
