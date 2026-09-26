/**
 * The editor (EditorPane.kt): the open files as tabs, the find bar when it is open, and the active
 * file's text. The editors themselves live in the workspace's host element, which is attached
 * here while the page shows and kept when it does not.
 */
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { ToolbarIcon } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { markAll, markedCount, search, type FindQuery } from "./EditorHost";
import { HairLine } from "./IdeParts";
import type { EditorTab, IdeWorkspace } from "./IdeWorkspace";

export function EditorPane({ ws, onClose }: { ws: IdeWorkspace; onClose: (tab: EditorTab) => void }) {
  const area = useRef<HTMLDivElement>(null);
  const active = ws.active;
  useLayoutEffect(() => {
    const el = area.current;
    if (!el) return;
    el.appendChild(ws.host);
    ws.active?.view.requestMeasure();
    return () => {
      ws.host.remove();
    };
  }, [ws]);
  return (
    <div className="ide-editor-pane">
      <div className="ide-tabs">
        {ws.tabs.map((tab) => (
          <Tab key={tab.key} tab={tab} active={tab === active} onClick={() => ws.activate(tab)} onClose={() => onClose(tab)} />
        ))}
      </div>
      <HairLine />
      {ws.findOpen && active && <FindBar key="find" tab={active} focus={ws.findFocus} onClose={() => ws.setFindOpen(false)} />}
      <div ref={area} className="ide-editor-area">
        {!active && <EmptyEditor />}
      </div>
    </div>
  );
}

function EmptyEditor() {
  return (
    <div className="ide-empty-editor">
      <div className="subtitle2 ide-empty-editor__title">Open a file from the explorer.</div>
      <div className="caption ide-empty-editor__hint">
        Ctrl+S saves it, Ctrl+F finds in it, Ctrl+Z takes an edit back. Ask Nook for a change in the panel on the right.
      </div>
    </div>
  );
}

// ====================================================================== tabs

function Tab({ tab, active, onClick, onClose }: { tab: EditorTab; active: boolean; onClick: () => void; onClose: () => void }) {
  return (
    <div className={active ? "ide-tab ide-tab--active" : "ide-tab"} onClick={onClick}>
      <span className="ide-tab__name body2">{tab.name}</span>
      <span className="ide-tab__end">
        <button
          type="button"
          className="ide-tab__close"
          aria-label="Close"
          title="Close"
          onClick={(e) => {
            e.stopPropagation();
            onClose();
          }}
        >
          <Icon name="close" size={10} />
        </button>
        {tab.modified && <span className={tab.conflict ? "ide-tab__dot ide-tab__dot--conflict" : "ide-tab__dot"} />}
      </span>
    </div>
  );
}

// ====================================================================== find

/** Finds text in the active file: typing moves to the first match, Enter to the next, Shift+Enter back, Escape closes. */
function FindBar({ tab, focus, onClose }: { tab: EditorTab; focus: number; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const [matchCase, setMatchCase] = useState(false);
  const [, setCount] = useState(-1);
  const input = useRef<HTMLInputElement>(null);
  const q: FindQuery = { query, matchCase };

  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, [focus]);

  useEffect(() => {
    const view = tab.view;
    setCount(search(view, { query, matchCase }, true, true));
    return () => {
      // The marks go with the bar, or with the tab it was searching.
      markAll(view, null);
    };
  }, [query, matchCase, tab]);

  const next = (forward: boolean) => setCount(search(tab.view, q, forward, false));
  // Read live, so edits to the text show in the count.
  const count = query ? markedCount(tab.view) : -1;

  return (
    <div className="ide-find">
      <Icon name="search" size={14} className="ide-find__icon" />
      <input
        ref={input}
        className="ide-find__input"
        value={query}
        placeholder={`Find in ${tab.name}`}
        spellCheck={false}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            next(!e.shiftKey);
          } else if (e.key === "Escape") {
            e.preventDefault();
            onClose();
            tab.view.focus();
          }
        }}
      />
      <span className={count === 0 ? "caption ide-find__count ide-find__count--none" : "caption ide-find__count"}>
        {count < 0 ? "" : count === 0 ? "No matches" : count === 1 ? "1 match" : `${count} matches`}
      </span>
      <button
        type="button"
        className={matchCase ? "ide-case ide-case--on" : "ide-case"}
        title="Match case"
        onClick={() => setMatchCase(!matchCase)}
      >
        Aa
      </button>
      <ToolbarIcon icon="arrow-up" hint="Previous match" onClick={() => next(false)} />
      <ToolbarIcon icon="arrow-down" hint="Next match" onClick={() => next(true)} />
      <ToolbarIcon icon="close" hint="Close" onClick={onClose} />
    </div>
  );
}
