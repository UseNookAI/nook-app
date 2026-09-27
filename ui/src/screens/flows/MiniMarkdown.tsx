/**
 * The Markdown a chat model writes for a summary or notes, drawn as React elements: headings,
 * paragraphs, bullet and numbered lists, **bold**, *italic* and `code`. Nothing of it is ever
 * put in as HTML, so a reply cannot bring its own markup into the app.
 */
import type { ReactNode } from "react";

export type Block =
  | { kind: "heading"; level: 1 | 2 | 3; text: string }
  | { kind: "paragraph"; text: string }
  | { kind: "list"; ordered: boolean; items: string[] };

/** The blocks of `markdown`, in order. */
export function markdownBlocks(markdown: string): Block[] {
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;
  const flush = () => {
    if (paragraph.length) blocks.push({ kind: "paragraph", text: paragraph.join(" ") });
    paragraph = [];
    if (list) blocks.push({ kind: "list", ...list });
    list = null;
  };
  for (const raw of markdown.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || /^([-*_])\1{2,}$/.test(line) || line.startsWith("```")) {
      flush();
      continue;
    }
    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      flush();
      const level = Math.min(3, heading[1].length) as 1 | 2 | 3;
      blocks.push({ kind: "heading", level, text: heading[2].replace(/\s*#+$/, "") });
      continue;
    }
    const bullet = /^[-*+•]\s+(.*)$/.exec(line);
    const numbered = /^\d{1,3}[.)]\s+(.*)$/.exec(line);
    const item = bullet ?? numbered;
    if (item) {
      if (paragraph.length) {
        blocks.push({ kind: "paragraph", text: paragraph.join(" ") });
        paragraph = [];
      }
      const ordered = numbered != null && bullet == null;
      if (list && list.ordered !== ordered) {
        blocks.push({ kind: "list", ...list });
        list = null;
      }
      list ??= { ordered, items: [] };
      list.items.push(item[1]);
      continue;
    }
    if (list && /^\s{2,}/.test(raw)) {
      // A line carried on under a list item.
      list.items[list.items.length - 1] += ` ${line}`;
      continue;
    }
    if (list) {
      blocks.push({ kind: "list", ...list });
      list = null;
    }
    paragraph.push(line.replace(/^>\s?/, ""));
  }
  flush();
  return blocks;
}

export type Span = { text: string; bold?: boolean; italic?: boolean; code?: boolean };

/** A line's **bold**, *italic* and `code` runs. */
export function inlineSpans(text: string): Span[] {
  const spans: Span[] = [];
  const pattern = /(\*\*|__)(.+?)\1|`([^`]+)`|(\*|_)(?!\s)(.+?)(?<!\s)\4(?![A-Za-z0-9])/g;
  let at = 0;
  for (let m = pattern.exec(text); m; m = pattern.exec(text)) {
    if (m.index > at) spans.push({ text: text.slice(at, m.index) });
    if (m[2] != null) spans.push({ text: m[2], bold: true });
    else if (m[3] != null) spans.push({ text: m[3], code: true });
    else spans.push({ text: m[5], italic: true });
    at = m.index + m[0].length;
  }
  if (at < text.length) spans.push({ text: text.slice(at) });
  return spans;
}

function Inline({ text }: { text: string }) {
  return (
    <>
      {inlineSpans(text).map((s, i): ReactNode => {
        if (s.bold) return <strong key={i}>{s.text}</strong>;
        if (s.italic) return <em key={i}>{s.text}</em>;
        if (s.code) return <code key={i}>{s.text}</code>;
        return <span key={i}>{s.text}</span>;
      })}
    </>
  );
}

export function MiniMarkdown({ text }: { text: string }) {
  return (
    <div className="nl-md selectable">
      {markdownBlocks(text).map((b, i) => {
        if (b.kind === "heading") {
          const cls = b.level === 1 ? "subtitle1 nl-md__h1" : "subtitle2 nl-md__h2";
          return (
            <div key={i} className={cls} role="heading" aria-level={b.level}>
              <Inline text={b.text} />
            </div>
          );
        }
        if (b.kind === "paragraph") {
          return (
            <p key={i} className="body2 nl-md__p">
              <Inline text={b.text} />
            </p>
          );
        }
        const items = b.items.map((it, j) => (
          <li key={j} className="body2">
            <Inline text={it} />
          </li>
        ));
        return b.ordered ? (
          <ol key={i} className="nl-md__list">
            {items}
          </ol>
        ) : (
          <ul key={i} className="nl-md__list">
            {items}
          </ul>
        );
      })}
    </div>
  );
}
