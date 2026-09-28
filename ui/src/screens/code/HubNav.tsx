/**
 * Chat, Code and Nooklets in the title strip, an icon each on one track: which page the hub
 * shows. Chat starts afresh (the start page, set to a chat) and stays lit while a conversation or
 * a video is open; Code opens the editor, Nooklets the finder of the ready-made jobs.
 */
import { useSyncExternalStore } from "react";
import { Icon } from "../../components/Icon";
import { FlowMemory } from "../flows/memory";
import { CodeMemory } from "./CodeHub";

type Page = "chat" | "code" | "flows";

const PAGES: { page: Page; icon: string; label: string; open: () => void }[] = [
  { page: "chat", icon: "chat", label: "Chat", open: () => CodeMemory.set({ code: false, flows: false, kind: "CHAT", selected: null }) },
  { page: "code", icon: "code", label: "Code", open: () => CodeMemory.set({ code: true, flows: false }) },
  {
    page: "flows",
    icon: "nooklets",
    label: "Nooklets",
    open: () => {
      FlowMemory.set("HOME");
      CodeMemory.set({ flows: true, code: false });
    },
  },
];

export function HubNav() {
  const memory = useSyncExternalStore(CodeMemory.subscribe, CodeMemory.get);
  const current: Page = memory.flows ? "flows" : memory.code ? "code" : "chat";
  return (
    <nav className="nk-topbar-nav" aria-label="Pages">
      {PAGES.map((p) => (
        <button
          key={p.page}
          type="button"
          className={p.page === current ? "nk-topbar-nav__item nk-topbar-nav__item--active" : "nk-topbar-nav__item"}
          title={p.label}
          aria-label={p.label}
          aria-current={p.page === current ? "page" : undefined}
          onClick={p.open}
        >
          <Icon name={p.icon} size={16} />
        </button>
      ))}
    </nav>
  );
}
