/**
 * CodeHub.kt: the app under the title strip. The sidebar on the left (the sessions), and on the
 * right the open page: a session, the Chat start page with its Chat | Video switch, the Code page,
 * or the Nooklets page, as the title strip's Chat, Code and Nooklets (HubNav) or a session
 * choose. The local worker does the changes; the person reads them, asks for more, applies or
 * discards.
 */
import { useSyncExternalStore } from "react";
import { codeDelete, codeRename } from "../../api/code";
import { FlowsScreen } from "../flows/FlowsScreen";
import { IdeScreen } from "../ide/IdeScreen";
import type { ChatKind } from "../hub/KindSwitch";
import { VideoScreen } from "../video/VideoScreen";
import { CodeSessionScreen, CodeStartScreen } from "./CodeSessionScreen";
import { CodeSidebar } from "./CodeSidebar";
import { useCodeActions, useCodeSnapshot } from "./useCode";
import "./code.css";

export interface CodeHubProps {
  isSidebarCollapsed: boolean;
  onToggleSidebar: () => void;
  /** Opens Settings on a tab: "general", "models", "models/code", "runtime", "about". */
  onSettings: (tabId: string) => void;
}

// ------------------------------------------------------------------ CodeMemory

interface Memory {
  selected: string | null;
  kind: ChatKind;
  code: boolean;
  flows: boolean;
}

/**
 * Which page is open, kept while the hub is rendered again or mounted afresh (after the welcome
 * or erase screens): a session, the Chat start page with its kind (a chat or a video), the Code
 * page, or the Nooklets page.
 */
export const CodeMemory = (() => {
  let state: Memory = { selected: null, kind: "CHAT", code: false, flows: false };
  const listeners = new Set<() => void>();
  return {
    get: () => state,
    set(patch: Partial<Memory>) {
      state = { ...state, ...patch };
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

const useMemory = () => useSyncExternalStore(CodeMemory.subscribe, CodeMemory.get);

// ------------------------------------------------------------------ the hub

export function CodeHub({ isSidebarCollapsed, onSettings }: CodeHubProps) {
  const snapshot = useCodeSnapshot();
  const actions = useCodeActions();
  const memory = useMemory();
  const onOpenModels = () => onSettings("models/code");
  const open = snapshot.sessions.find((s) => s.id === memory.selected) ?? null;

  let page;
  if (memory.flows) {
    page = <FlowsScreen say={actions.say} onOpenModels={onOpenModels} />;
  } else if (memory.code) {
    page = <IdeScreen onOpenModels={onOpenModels} />;
  } else if (open != null) {
    page = <CodeSessionScreen key={open.id} session={open} snapshot={snapshot} actions={actions} onOpenModels={onOpenModels} />;
  } else if (memory.kind === "VIDEO") {
    page = <VideoScreen say={actions.say} onChat={() => CodeMemory.set({ kind: "CHAT" })} />;
  } else {
    page = (
      <CodeStartScreen
        snapshot={snapshot}
        actions={actions}
        onStarted={(id) => CodeMemory.set({ selected: id })}
        onOpenModels={onOpenModels}
        kind={memory.kind}
        onKind={(kind) => CodeMemory.set({ kind })}
      />
    );
  }

  return (
    <div className="nc-hub">
      <CodeSidebar
        sessions={snapshot.sessions}
        selectedId={memory.flows || memory.code ? null : memory.selected}
        isCollapsed={isSidebarCollapsed}
        onOpen={(id) => CodeMemory.set({ code: false, flows: false, selected: id })}
        onDelete={(id) => {
          if (CodeMemory.get().selected === id) CodeMemory.set({ selected: null });
          actions.run(() => codeDelete(id));
        }}
        onRename={(id, title) => actions.run(() => codeRename(id, title))}
        onSettings={() => onSettings("general")}
      />
      <div className="nc-hub__page">{page}</div>
    </div>
  );
}
