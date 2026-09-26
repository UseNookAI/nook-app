/**
 * The editors (EditorHost.kt, the Editors object): one CodeMirror view per open file, each in its
 * own card inside one host element that outlives the page, so leaving Code and coming back finds
 * every editor as it was, unsaved edits and undo history included. Also the colours (from
 * theme.css through the --ide-* variables in ide.css) and the find bar's search.
 */
import { defaultKeymap, history, historyKeymap, indentLess, indentMore } from "@codemirror/commands";
import {
  bracketMatching,
  foldGutter,
  foldKeymap,
  HighlightStyle,
  indentOnInput,
  indentUnit,
  syntaxHighlighting,
  type LanguageSupport,
} from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { highlightSelectionMatches, SearchCursor } from "@codemirror/search";
import {
  Annotation,
  Compartment,
  EditorSelection,
  EditorState,
  Prec,
  StateEffect,
  StateField,
  Transaction,
  type Extension,
  type Text,
} from "@codemirror/state";
import {
  Decoration,
  drawSelection,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  type Command,
  type DecorationSet,
} from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import type { Language } from "./IdeSupport";

/** What an editor tells the Code page. */
export interface EditorEvents {
  changed: () => void;
  caret: (line: number, column: number) => void;
  save: () => void;
  find: () => void;
}

// ---------------------------------------------------------------------------- colours

/**
 * The token colours of the original's palette (Editors.apply): keywords and markup tags green,
 * types and attributes lavender, strings amber, numbers red, comments quiet, annotations lavender,
 * operators secondary. Everything else is the text colour.
 */
const nookHighlight = HighlightStyle.define([
  { tag: [t.keyword, t.tagName, t.heading], color: "var(--ide-keyword)" },
  { tag: t.heading, fontWeight: "600" },
  { tag: [t.typeName, t.className, t.namespace, t.attributeName, t.link], color: "var(--ide-type)" },
  { tag: [t.string, t.docString, t.character, t.attributeValue, t.regexp, t.special(t.string), t.url], color: "var(--ide-string)" },
  { tag: [t.number, t.bool, t.atom, t.escape, t.color, t.invalid], color: "var(--ide-number)" },
  { tag: [t.comment, t.lineComment, t.blockComment, t.docComment], color: "var(--ide-comment)" },
  {
    tag: [t.meta, t.annotation, t.processingInstruction, t.documentMeta, t.macroName, t.special(t.variableName)],
    color: "var(--ide-meta)",
  },
  { tag: [t.operator, t.punctuation, t.separator, t.bracket, t.angleBracket], color: "var(--ide-operator)" },
  { tag: t.emphasis, fontStyle: "italic" },
  { tag: t.strong, fontWeight: "600" },
  { tag: t.strikethrough, textDecoration: "line-through" },
]);

const nookTheme = EditorView.theme({
  "&": { height: "100%", color: "var(--ide-text)", backgroundColor: "var(--ide-bg)" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "var(--font-mono)", fontSize: "13px", lineHeight: "20px" },
  ".cm-content": { padding: "6px 0", caretColor: "var(--ide-text)" },
  ".cm-line": { padding: "0 8px" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--ide-text)", borderLeftWidth: "1.5px" },
  ".cm-gutters": {
    backgroundColor: "var(--ide-gutter)",
    color: "var(--ide-gutter-text)",
    borderRight: "var(--border-width) solid var(--border)",
  },
  ".cm-lineNumbers .cm-gutterElement": { padding: "0 4px 0 10px", fontSize: "12px", minWidth: "36px" },
  ".cm-activeLineGutter": { backgroundColor: "transparent", color: "var(--ide-text)" },
  ".cm-activeLine": { backgroundColor: "var(--ide-current-line)" },
  ".cm-selectionBackground, &.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground": {
    backgroundColor: "var(--ide-selection)",
  },
  ".cm-selectionMatch": { backgroundColor: "var(--ide-mark)" },
  ".cm-ide-match": { backgroundColor: "var(--ide-find)" },
  ".cm-ide-match-current, .cm-ide-match .cm-ide-match-current": {
    backgroundColor: "var(--ide-find-current)",
    outline: "var(--border-width) solid var(--warning)",
  },
  "&.cm-focused .cm-matchingBracket, .cm-matchingBracket": {
    backgroundColor: "var(--ide-bracket)",
    outline: "var(--border-width) solid var(--ide-keyword)",
  },
  "&.cm-focused .cm-nonmatchingBracket": { backgroundColor: "transparent" },
  ".cm-foldGutter .cm-gutterElement": { padding: "0 4px", color: "var(--ide-gutter-text)", cursor: "pointer" },
  ".cm-foldGutter .cm-gutterElement:hover": { color: "var(--ide-text)" },
  ".cm-foldPlaceholder": {
    backgroundColor: "var(--ide-mark)",
    border: "none",
    color: "var(--ide-gutter-text)",
    padding: "0 4px",
    borderRadius: "4px",
  },
  ".cm-specialChar": { color: "var(--ide-number)" },
});

/** The fold arrow: right at a folded block, down at an open one (RSyntaxTextArea's MODERN style). */
function foldMarker(open: boolean): HTMLElement {
  const span = document.createElement("span");
  span.className = "ide-fold-marker";
  span.style.transform = open ? "rotate(90deg)" : "none";
  span.innerHTML =
    '<svg width="10" height="10" viewBox="0 0 10 10" fill="none"><path d="M3.6 2.2 6.6 5 3.6 7.8" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/></svg>';
  return span;
}

// ---------------------------------------------------------------------------- languages

const loaded = new Map<string, Promise<LanguageSupport | null>>();

/** The CodeMirror support for a language-data mode, loaded once; null when there is none. */
export function loadLanguage(mode: string | null): Promise<LanguageSupport | null> {
  if (!mode) return Promise.resolve(null);
  let p = loaded.get(mode);
  if (!p) {
    const desc = languages.find((l) => l.name === mode);
    p = desc ? desc.load().catch(() => null) : Promise.resolve(null);
    loaded.set(mode, p);
  }
  return p;
}

// ---------------------------------------------------------------------------- find

export interface FindQuery {
  query: string;
  matchCase: boolean;
}

interface FindState {
  query: FindQuery | null;
  marks: DecorationSet;
  /** How many matches the text holds; -1 for no query. */
  count: number;
}

const setFind = StateEffect.define<FindQuery | null>();
const matchMark = Decoration.mark({ class: "cm-ide-match" });
const currentMark = Decoration.mark({ class: "cm-ide-match-current" });
/** Marking stops here in a huge file; the count still counts every match. */
const MAX_MARKS = 20000;

function cursorFor(doc: Text, q: FindQuery, from = 0, to = doc.length): SearchCursor {
  return new SearchCursor(doc, q.query, from, to, q.matchCase ? undefined : (s) => s.toLowerCase());
}

function computeFind(doc: Text, query: FindQuery | null): FindState {
  if (!query || !query.query) return { query, marks: Decoration.none, count: -1 };
  const ranges = [];
  let count = 0;
  for (const c = cursorFor(doc, query); !c.next().done; ) {
    if (count < MAX_MARKS) ranges.push(matchMark.range(c.value.from, c.value.to));
    count++;
  }
  return { query, marks: Decoration.set(ranges), count };
}

const findField = StateField.define<FindState>({
  create: () => ({ query: null, marks: Decoration.none, count: -1 }),
  update(value, tr) {
    let query = value.query;
    let set = false;
    for (const e of tr.effects) {
      if (e.is(setFind)) {
        query = e.value;
        set = true;
      }
    }
    if (!set && !tr.docChanged) return value;
    return computeFind(tr.state.doc, query);
  },
  provide: (f) => [
    EditorView.decorations.from(f, (v) => v.marks),
    // The match the find bar moved to is the selection; it is marked above the others.
    EditorView.decorations.compute([f, "selection"], (state) => {
      const { query } = state.field(f);
      const sel = state.selection.main;
      if (!query || !query.query || sel.empty) return Decoration.none;
      const text = state.sliceDoc(sel.from, sel.to);
      const same = query.matchCase ? text === query.query : text.toLowerCase() === query.query.toLowerCase();
      return same ? Decoration.set([currentMark.range(sel.from, sel.to)]) : Decoration.none;
    }),
  ],
});

/** Marks every match of the query in the view (or clears the marks); returns how many there are. */
export function markAll(view: EditorView, query: FindQuery | null): number {
  view.dispatch({ effects: setFind.of(query && query.query ? query : null) });
  return view.state.field(findField).count;
}

/** How many matches are marked now; -1 for no query. */
export function markedCount(view: EditorView): number {
  return view.state.field(findField, false)?.count ?? -1;
}

/**
 * Finds the query from the caret, forward or back and round the end, and selects the match.
 * `fromSelection` searches from the start of the selection, so a query typed letter by letter
 * stays on the same match. Returns how many matches the text holds; -1 for no query.
 */
export function search(view: EditorView, q: FindQuery, forward: boolean, fromSelection: boolean): number {
  const count = markAll(view, q);
  if (!q.query) return -1;
  const { doc, selection } = view.state;
  const sel = selection.main;
  const start = fromSelection || !forward ? sel.from : sel.to;
  let match: { from: number; to: number } | null = null;
  if (forward) {
    const after = cursorFor(doc, q, start).next();
    const wrapped = after.done ? cursorFor(doc, q).next() : after;
    if (!wrapped.done) match = wrapped.value;
  } else {
    let last: { from: number; to: number } | null = null;
    for (const c = cursorFor(doc, q, 0, start); !c.next().done; ) last = c.value;
    if (!last) for (const c = cursorFor(doc, q, start); !c.next().done; ) last = c.value;
    match = last;
  }
  if (match) {
    view.dispatch({
      selection: EditorSelection.single(match.from, match.to),
      effects: EditorView.scrollIntoView(match.from, { y: "center" }),
    });
  }
  return count;
}

// ---------------------------------------------------------------------------- the editor

/** A change the page makes itself (reading a file again), which does not count as an edit. */
const reload = Annotation.define<boolean>();

/** Tab: spaces to the next stop (or a real tab), or indents the selected lines. */
function tabKey(useTabs: boolean): Command {
  return (view) => {
    const { state } = view;
    if (state.selection.ranges.some((r) => !r.empty)) return indentMore(view);
    view.dispatch(
      state.changeByRange((range) => {
        const line = state.doc.lineAt(range.head);
        const column = range.head - line.from;
        const insert = useTabs ? "\t" : " ".repeat(4 - (column % 4));
        return { changes: { from: range.head, insert }, range: EditorSelection.cursor(range.head + insert.length) };
      }),
    );
    return true;
  };
}

export interface EditorHandle {
  view: EditorView;
  /** The card the view sits in, shown when its tab is active. */
  card: HTMLElement;
  /** Puts text read again from disk in the editor: no edit, no undo step, the undo history cleared. */
  replaceText: (text: string) => void;
}

/** An editor for `text` in `language`, with line numbers and folding, in a card of `host`. */
export function createEditor(host: HTMLElement, text: string, language: Language, events: EditorEvents): EditorHandle {
  const languageConf = new Compartment();
  const historyConf = new Compartment();
  const extensions: Extension[] = [
    Prec.highest(
      keymap.of([
        { key: "Mod-s", run: () => (events.save(), true), preventDefault: true },
        { key: "Mod-f", run: () => (events.find(), true), preventDefault: true },
        { key: "Tab", run: tabKey(language.tabs), shift: indentLess },
      ]),
    ),
    lineNumbers(),
    foldGutter({ markerDOM: foldMarker }),
    highlightActiveLineGutter(),
    highlightSpecialChars(),
    historyConf.of(history()),
    drawSelection(),
    indentOnInput(),
    bracketMatching(),
    highlightActiveLine(),
    highlightSelectionMatches({ highlightWordAroundCursor: true, minSelectionLength: 2 }),
    EditorState.tabSize.of(4),
    indentUnit.of(language.tabs ? "\t" : "    "),
    syntaxHighlighting(nookHighlight),
    nookTheme,
    findField,
    languageConf.of([]),
    keymap.of([...defaultKeymap, ...historyKeymap, ...foldKeymap]),
    EditorView.updateListener.of((u) => {
      if (u.docChanged && !u.transactions.every((tr) => tr.annotation(reload))) events.changed();
      if (u.docChanged || u.selectionSet) {
        const head = u.state.selection.main.head;
        const line = u.state.doc.lineAt(head);
        events.caret(line.number, head - line.from + 1);
      }
    }),
  ];
  const card = document.createElement("div");
  card.className = "ide-editor-card";
  host.appendChild(card);
  const view = new EditorView({ state: EditorState.create({ doc: text, extensions }), parent: card });
  loadLanguage(language.mode).then((support) => {
    if (support) view.dispatch({ effects: languageConf.reconfigure(support) });
  });

  const replaceText = (next: string) => {
    const old = view.state.doc.toString();
    if (old !== next) {
      // Only the part that differs is replaced, so the caret, the folds and the scroll stay put.
      let start = 0;
      const max = Math.min(old.length, next.length);
      while (start < max && old.charCodeAt(start) === next.charCodeAt(start)) start++;
      let endOld = old.length;
      let endNew = next.length;
      while (endOld > start && endNew > start && old.charCodeAt(endOld - 1) === next.charCodeAt(endNew - 1)) {
        endOld--;
        endNew--;
      }
      view.dispatch({
        changes: { from: start, to: endOld, insert: next.slice(start, endNew) },
        annotations: [reload.of(true), Transaction.addToHistory.of(false)],
      });
    }
    // A fresh history: what was read cannot be undone into what was there before.
    view.dispatch({ effects: historyConf.reconfigure([]), annotations: reload.of(true) });
    view.dispatch({ effects: historyConf.reconfigure(history()), annotations: reload.of(true) });
  };
  return { view, card, replaceText };
}
