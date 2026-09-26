/**
 * FlowsScreen.kt: ready-made jobs, done in steps on this computer. The flows in a rail on the
 * left, the open one on the right with its runs.
 */
import { useState, useSyncExternalStore } from "react";
import { Icon } from "../../components/Icon";
import { PagePane } from "../hub/PagePane";
import { PdfFlow } from "./PdfFlow";
import { TranslateFlow } from "./TranslateFlow";
import "./flows.css";

/** The flows Nook offers, in the rail's order. */
export type FlowKind = "TRANSLATE" | "PDF";

const FLOWS: { kind: FlowKind; title: string; icon: string; blurb: string }[] = [
  {
    kind: "TRANSLATE",
    title: "Translate speech",
    icon: "translate",
    blurb: "Speak or drop a file; hear it in another language",
  },
  {
    kind: "PDF",
    title: "Edit a PDF",
    icon: "file-edit",
    blurb: "Change any text; the font stays the same",
  },
];

/** The open flow, kept while the hub is rendered again (FlowMemory.flow). */
const FlowMemory = (() => {
  let flow: FlowKind = "TRANSLATE";
  const listeners = new Set<() => void>();
  return {
    get: () => flow,
    set(next: FlowKind) {
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

export function FlowsScreen({ say, onOpenModels }: { say: (message: string) => void; onOpenModels: () => void }) {
  const flow = useSyncExternalStore(FlowMemory.subscribe, FlowMemory.get);
  // An open PDF wants the room: the rail folds to its icons.
  const [narrow, setNarrow] = useState(false);
  return (
    <PagePane>
      <div className="fl-page">
        <nav className={narrow && flow === "PDF" ? "fl-rail fl-rail--narrow" : "fl-rail"}>
          <div className="h6 fl-rail__title-text">Flows</div>
          <div className="caption text-tertiary fl-rail__blurb">Ready-made jobs that run on your own GPU, step by step.</div>
          <div className="fl-rail__list">
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
          {flow === "TRANSLATE" && <TranslateFlow say={say} onOpenModels={onOpenModels} />}
          {flow === "PDF" && <PdfFlow say={say} onOpenChange={setNarrow} />}
        </div>
      </div>
    </PagePane>
  );
}
