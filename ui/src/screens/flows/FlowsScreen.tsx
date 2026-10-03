/**
 * FlowsScreen.kt, now the Nooklets page: ready-made jobs, done in steps on this computer. It opens
 * on the finder (NookletsHome), which asks what the person wants done; the Nooklet it opens fills
 * the page. The Nooklets, the way back to the finder and the recent runs are in the sidebar
 * (NookletsSidebar).
 */
import { useState, useSyncExternalStore } from "react";
import type { NookletId, Preset } from "../../api/nooklets";
import { PagePane } from "../hub/PagePane";
import { ConvertFlow } from "./ConvertFlow";
import { NookletsHome } from "./NookletsHome";
import { PdfFlow } from "./PdfFlow";
import { ReadAloudFlow } from "./ReadAloudFlow";
import { ScreenFlow } from "./ScreenFlow";
import { SummarizeFlow } from "./SummarizeFlow";
import { TranscribeFlow } from "./TranscribeFlow";
import { TranslateFlow } from "./TranslateFlow";
import { FlowMemory, NOOKLETS, type FlowKind } from "./memory";
import "./flows.css";
import "./nooklets.css";

export type { FlowKind } from "./memory";

export { FlowMemory };

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

  const open = (id: NookletId, preset: Preset | null) => {
    const kind = NOOKLETS.find((f) => f.id === id)?.kind ?? "TRANSLATE";
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
  const languageFor = (kind: FlowKind) => {
    const a = presetFor(kind);
    return a ? { language: a.preset.value, nonce: a.nonce } : null;
  };
  const format = presetFor("CONVERT");
  return (
    <PagePane>
      <div className="fl-page">
        <div className="fl-main">
          {flow === "TRANSLATE" && <TranslateFlow say={say} onOpenModels={onOpenModels} preset={languageFor("TRANSLATE")} />}
          {flow === "TRANSCRIBE" && <TranscribeFlow say={say} onOpenModels={onOpenModels} preset={languageFor("TRANSCRIBE")} />}
          {flow === "SUMMARIZE" && <SummarizeFlow say={say} onOpenModels={onOpenModels} preset={languageFor("SUMMARIZE")} />}
          {flow === "READ_ALOUD" && <ReadAloudFlow say={say} preset={languageFor("READ_ALOUD")} />}
          {flow === "PDF" && <PdfFlow say={say} />}
          {flow === "SCREEN" && (
            <ScreenFlow say={say} preset={presetFor("SCREEN") ? { mode: presetFor("SCREEN")!.preset.value, nonce: presetFor("SCREEN")!.nonce } : null} />
          )}
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
