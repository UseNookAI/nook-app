/**
 * The Chat | Video switch (KindSwitch.kt): two pills in one rounded track, the chosen one raised on
 * the surface colour.
 */
import { Icon } from "../../components/Icon";
import "./hub.css";

/** What the Chat page makes: a conversation with the code worker, or a video clip. */
export type ChatKind = "CHAT" | "VIDEO";

export function KindSwitch({ kind, onKind }: { kind: ChatKind; onKind: (kind: ChatKind) => void }) {
  return (
    <div className="nk-kind-switch" role="tablist">
      <KindPill text="Chat" icon="new-chat" selected={kind === "CHAT"} onClick={() => onKind("CHAT")} />
      <KindPill text="Video" icon="video" selected={kind === "VIDEO"} onClick={() => onKind("VIDEO")} />
    </div>
  );
}

function KindPill({ text, icon, selected, onClick }: { text: string; icon: string; selected: boolean; onClick: () => void }) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={selected}
      className={selected ? "nk-kind-pill nk-kind-pill--selected" : "nk-kind-pill"}
      onClick={onClick}
    >
      <Icon name={icon} size={14} />
      <span>{text}</span>
    </button>
  );
}
