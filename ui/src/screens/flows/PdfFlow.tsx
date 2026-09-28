/**
 * The PDF editor flow: open a PDF, drag across the words to change (or click a line), type, and
 * press Enter. The text becomes an editing box in its own font, size and colour, sitting on the
 * page's own background; the new text goes into the PDF as text, in the same font (from Windows
 * when the PDF carries too few of its letters). Text that is part of a picture (a scan) is read by
 * Windows, and its new text goes over it in the closest installed font, or, the other choice, made
 * of letters cut from the page itself. Pages draw as they scroll into view, so a long document
 * opens at once. Saving writes "<name> (edited).pdf" beside the original, or where
 * Save as says.
 */
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from "react";
import { messageOf } from "../../api/ipc";
import type { Install } from "../../api/flows";
import {
  chooseSavePath,
  choosePdf,
  onPdf,
  pdfCancelInstall,
  pdfClearInstallError,
  pdfClose,
  pdfDocs,
  pdfInstall,
  pdfOpen,
  pdfPick,
  pdfRender,
  pdfReplace,
  pdfReveal,
  pdfSave,
  pdfSetup,
  pdfUndo,
  type Align,
  type Area,
  type Block,
  type PdfDoc,
  type PdfPageInfo,
  type PdfSetup,
} from "../../api/pdf";
import { QuietAction, TextLink } from "../../components/Activity";
import { Button, IconButton } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { Icon } from "../../components/Icon";
import { Spinner } from "../../components/Spinner";
import { DownloadLine } from "./DownloadLine";
import { splitPath } from "./format";
import { useFileDrop } from "./useFileDrop";
import { registerLeaveCheck } from "../../shell/unsaved";
import "./pdf.css";

/** The open document, kept while the page is left and opened again. */
let rememberedDoc: string | null = null;
/** Whether the edit box holds a change not put in yet; closing the window asks about it. */
let draftOpen = false;
registerLeaveCheck("pdf", () => (draftOpen ? ["A change typed into a PDF is not put in yet"] : []));

/** The widest a page is drawn at 100%, in CSS pixels. */
const MAX_PAGE_WIDTH = 980;
const ZOOMS = [0.5, 0.67, 0.8, 1, 1.25, 1.5, 2];

interface Editing {
  block: Block;
  text: string;
  align: Align;
  /** The page's colour behind the text, so the box covers the old words. */
  background: string;
  /** Text in a picture: made of the page's own letters rather than set in the matched font. */
  fromPage: boolean;
}

interface Drag {
  page: number;
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

export function PdfFlow({ say, onOpenChange }: { say: (message: string) => void; onOpenChange?: (open: boolean) => void }) {
  const [setup, setSetup] = useState<PdfSetup | null>(null);
  const [install, setInstall] = useState<Install | null>(null);
  /** A PDF chosen before the engine was in: it opens as soon as the download is done. */
  const [pending, setPending] = useState<string | null>(null);
  const [doc, setDoc] = useState<PdfDoc | null>(null);
  const [opening, setOpening] = useState<string | null>(null);
  const [zoom, setZoom] = useState(1);
  const [editing, setEditing] = useState<Editing | null>(null);
  const [hint, setHint] = useState<{ page: number; x: number; y: number; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [asking, setAsking] = useState<null | (() => void)>(null);
  const [fit, setFit] = useState(800);
  const scroller = useRef<HTMLDivElement>(null);
  const images = useRef(new Map<number, HTMLImageElement>());

  useEffect(() => {
    let alive = true;
    const read = () =>
      pdfSetup().then(
        (s) => {
          if (!alive) return;
          setSetup(s);
          setInstall(s.install);
        },
        () => alive && setSetup({ installed: false, bytes: 0, install: null }),
      );
    read();
    // The download's progress; once it is over, whether the engine is in.
    const off = onPdf((e) => {
      if (e.install === undefined) return;
      setInstall(e.install);
      if (e.install == null) read();
    });
    return () => {
      alive = false;
      off();
    };
  }, []);

  useEffect(() => {
    // Back to the document that was open.
    if (rememberedDoc) {
      const id = rememberedDoc;
      pdfDocs().then((all) => {
        const open = all.find((d) => d.id === id);
        if (open) setDoc(open);
      }, () => undefined);
    }
  }, []);

  useEffect(() => {
    rememberedDoc = doc?.id ?? null;
    onOpenChange?.(doc != null);
  }, [doc?.id, doc, onOpenChange]);
  useEffect(() => () => onOpenChange?.(false), [onOpenChange]);
  useEffect(() => {
    draftOpen = editing != null && editing.text.replace(/\r/g, "") !== editing.block.lines.map((l) => l.text).join("\n");
  }, [editing]);
  useEffect(
    () => () => {
      draftOpen = false;
    },
    [],
  );

  // The width a page gets at 100%: the panel's, less its margins.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const measure = () => setFit(Math.min(MAX_PAGE_WIDTH, Math.max(320, el.clientWidth - 64)));
    measure();
    const o = new ResizeObserver(measure);
    o.observe(el);
    return () => o.disconnect();
  }, [doc?.id]);

  const open = useCallback(
    async (path: string) => {
      if (!/\.pdf$/i.test(path)) {
        say("That is not a PDF.");
        return;
      }
      if (!setup?.installed) {
        // The engine first, with its download line showing; the PDF opens once it is in.
        setPending(path);
        if (!install || install.error) pdfClearInstallError().then(pdfInstall).catch((e) => say(messageOf(e)));
        return;
      }
      try {
        setOpening(`Opening ${splitPath(path).name}…`);
        const opened = await pdfOpen(path);
        if (doc) pdfClose(doc.id).catch(() => undefined);
        setEditing(null);
        setZoom(1);
        setDoc(opened);
      } catch (e) {
        say(messageOf(e));
      } finally {
        setOpening(null);
      }
    },
    [setup, install, doc, say],
  );

  // A PDF chosen during the download opens when the engine is in.
  useEffect(() => {
    if (!pending || !setup?.installed) return;
    const path = pending;
    setPending(null);
    open(path);
  }, [pending, setup?.installed, open]);

  const chooseAndOpen = () => {
    choosePdf().then(
      (p) => {
        if (p) open(p);
      },
      () => undefined,
    );
  };

  const { hover } = useFileDrop(!busy && editing == null, (path) => {
    const go = () => open(path);
    if (doc?.dirty) setAsking(() => go);
    else go();
  });

  // ---------------------------------------------------------------- picking and replacing

  const pick = async (page: number, area: Area, at: { x: number; y: number }) => {
    if (!doc) return;
    setHint(null);
    // Text in a picture takes a moment to read.
    const slow = window.setTimeout(() => setHint({ page, x: at.x, y: at.y, text: "Reading the text…" }), 300);
    try {
      const p = await pdfPick(doc.id, page, area);
      window.clearTimeout(slow);
      if (!p.block) {
        setHint({ page, x: at.x, y: at.y, text: p.why ?? "There is no text there." });
        return;
      }
      setHint(null);
      const block = p.block;
      setEditing({
        block,
        text: block.lines.map((l) => l.text).join("\n"),
        align: block.align,
        background: block.drawn?.paper ?? backgroundAround(images.current.get(page), doc.pages[page], block),
        fromPage: false,
      });
    } catch (e) {
      window.clearTimeout(slow);
      setHint(null);
      say(messageOf(e));
    }
  };

  const replace = async () => {
    if (!doc || !editing || busy) return;
    const texts = editing.text.replace(/\r/g, "").split("\n");
    const same = texts.join("\n") === editing.block.lines.map((l) => l.text).join("\n");
    if (same) {
      setEditing(null);
      return;
    }
    setBusy(true);
    try {
      const { block } = editing;
      const chosen = block.drawn ? { ...block, drawn: { ...block.drawn, fromPage: editing.fromPage } } : block;
      const r = await pdfReplace(doc.id, chosen, texts, editing.align);
      setDoc(r.doc);
      setEditing(null);
      if (r.note) say(r.note);
    } catch (e) {
      say(messageOf(e));
    } finally {
      setBusy(false);
    }
  };

  const undo = useCallback(async () => {
    if (!doc?.canUndo || busy) return;
    setEditing(null);
    try {
      setDoc(await pdfUndo(doc.id));
    } catch (e) {
      say(messageOf(e));
    }
  }, [doc, busy, say]);

  const save = useCallback(
    async (as: boolean) => {
      if (!doc) return;
      let path: string | null = null;
      if (as) {
        const { folder } = splitPath(doc.path);
        const stem = doc.name.replace(/\.pdf$/i, "");
        path = await chooseSavePath(`${folder}\\${stem} (edited).pdf`);
        if (!path) return;
      }
      try {
        const saved = await pdfSave(doc.id, path);
        setDoc(saved);
        say(`Saved as ${splitPath(saved.savedTo ?? "").name}.`);
      } catch (e) {
        say(messageOf(e));
      }
    },
    [doc, say],
  );

  const close = () => {
    if (!doc) return;
    const go = () => {
      pdfClose(doc.id).catch(() => undefined);
      setDoc(null);
      setEditing(null);
    };
    if (doc.dirty) setAsking(() => go);
    else go();
  };

  // Ctrl+Z takes an edit back, Ctrl+S saves, Ctrl+plus and minus zoom; not while typing.
  useEffect(() => {
    if (!doc) return;
    const key = (e: KeyboardEvent) => {
      if (!e.ctrlKey || editing) return;
      const k = e.key.toLowerCase();
      if (k === "z") {
        e.preventDefault();
        undo();
      } else if (k === "s") {
        e.preventDefault();
        save(e.shiftKey);
      } else if (k === "=" || k === "+") {
        e.preventDefault();
        setZoom((z) => ZOOMS.find((v) => v > z + 0.01) ?? z);
      } else if (k === "-") {
        e.preventDefault();
        setZoom((z) => [...ZOOMS].reverse().find((v) => v < z - 0.01) ?? z);
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [doc, editing, undo, save]);

  // ---------------------------------------------------------------- the page

  if (!doc) {
    return (
      <div className="fl-flow">
        <div className="fl-column">
          <div className="fl-head">
            <div className="h5">Edit a PDF</div>
            <div className="body2 text-secondary">
              Open a PDF, drag across the words you want to change (or click a line), and type. The font, the size and the
              colour stay as they were, and it goes back in as real text. Scans too: text in a picture is read and set again in
              the closest font on this computer. Any length of document; it all stays on this computer.
            </div>
          </div>
          <div className="fl-card fl-form">
            {opening ? (
              <div className="fl-drop pdf-opening">
                <Spinner size={22} />
                <div className="subtitle2">{opening}</div>
              </div>
            ) : (
              <div
                className={hover ? "fl-drop fl-drop--hover" : "fl-drop"}
                role="button"
                tabIndex={0}
                onClick={chooseAndOpen}
                onKeyDown={(e) => e.key === "Enter" && chooseAndOpen()}
              >
                <Icon name="file-edit" size={26} color={hover ? "var(--primary-variant)" : "var(--text-secondary)"} />
                <div className="subtitle2">{hover ? "Drop it to open it" : "Drop a PDF here"}</div>
                <div className="caption text-tertiary">
                  {setup && !setup.installed ? "or click to choose one; it opens once the PDF engine below is in" : "or click to choose one"}
                </div>
              </div>
            )}
            {setup && !setup.installed && (
              <>
                {pending && (
                  <div className="caption text-tertiary">
                    {splitPath(pending).name} opens as soon as the PDF engine is in.
                  </div>
                )}
                <DownloadLine
                  text="One-time download: the PDF engine (Chrome's PDFium). It stays on this computer, and everything runs here."
                  bytes={setup.bytes || 3_733_154}
                  install={install}
                  onDownload={() => pdfInstall().catch((e) => say(messageOf(e)))}
                  onStop={() => {
                    setPending(null);
                    pdfCancelInstall().catch(() => undefined);
                  }}
                  onRetry={() => pdfClearInstallError().then(pdfInstall).catch((e) => say(messageOf(e)))}
                />
              </>
            )}
          </div>
        </div>
      </div>
    );
  }

  const pageWidth = fit * zoom;
  return (
    <div className="pdf-flow">
      <div className="pdf-bar">
        <span className="pdf-bar__icon">
          <Icon name="file-edit" size={16} />
        </span>
        <span className="pdf-bar__name">
          <span className="subtitle2 nc-ellipsis" title={doc.path}>
            {doc.name}
          </span>
          <span className="caption text-tertiary nc-ellipsis">
            {doc.pages.length} {doc.pages.length === 1 ? "page" : "pages"}
            {doc.edits > 0 ? ` · ${doc.edits} ${doc.edits === 1 ? "change" : "changes"}` : ""}
            {doc.dirty ? " · not saved" : doc.savedTo ? " · saved" : ""}
          </span>
        </span>
        <span className="nc-flex-spacer" />
        <div className="pdf-zoom">
          <IconButton icon="minus" size={28} iconSize={14} title="Smaller (Ctrl -)" onClick={() => setZoom((z) => [...ZOOMS].reverse().find((v) => v < z - 0.01) ?? z)} />
          <button type="button" className="numeric pdf-zoom__value" title="Fit the width" onClick={() => setZoom(1)}>
            {Math.round(zoom * 100)}%
          </button>
          <IconButton icon="plus" size={28} iconSize={14} title="Larger (Ctrl +)" onClick={() => setZoom((z) => ZOOMS.find((v) => v > z + 0.01) ?? z)} />
        </div>
        <QuietAction text="Undo" icon="undo" disabled={!doc.canUndo || busy} onClick={undo} title="Take the last change back (Ctrl Z)" />
        {doc.savedTo && !doc.dirty && <QuietAction text="Show" icon="folder-open" onClick={() => pdfReveal(doc.savedTo!).catch(() => undefined)} />}
        <QuietAction text="Save as…" onClick={() => save(true)} />
        <Button text="Save" icon="download" iconPosition="start" compact disabled={!doc.dirty} onClick={() => save(false)} />
        <IconButton icon="close" size={28} iconSize={14} title="Close the PDF" onClick={close} />
      </div>
      <div className="pdf-hint caption text-tertiary">
        Drag across the words to change, or click a line, scanned ones too. Enter puts the new text in; the font, size and colour
        stay the same.
      </div>
      <div className="pdf-scroll" ref={scroller}>
        {doc.pages.map((p, i) => (
          <PageView
            key={i}
            doc={doc}
            index={i}
            page={p}
            width={pageWidth}
            onImage={(img) => (img ? images.current.set(i, img) : images.current.delete(i))}
            onPick={pick}
            editing={editing?.block.page === i ? editing : null}
            onEdit={(patch) => setEditing((e) => (e ? { ...e, ...patch } : e))}
            onCancel={() => setEditing(null)}
            onReplace={replace}
            busy={busy}
            hint={hint?.page === i ? hint : null}
            onHintDone={() => setHint(null)}
            dismissEditing={() => setEditing(null)}
          />
        ))}
      </div>
      {hover && (
        <div className="pdf-drop-veil">
          <div className="subtitle2">Drop to open this PDF instead</div>
        </div>
      )}
      {asking && (
        <Dialog
          title="Leave the changes unsaved?"
          onDismiss={() => setAsking(null)}
          actions={
            <>
              <Button text="Keep editing" variant="ghost" onClick={() => setAsking(null)} />
              <Button
                text="Leave them"
                variant="danger"
                onClick={() => {
                  const go = asking;
                  setAsking(null);
                  go();
                }}
              />
            </>
          }
        >
          The changes to this PDF are not saved yet. Save first, or leave them out.
        </Dialog>
      )}
    </div>
  );
}

/** One page: its picture, drawn when it comes into view, and the layer that takes the pointer. */
function PageView({
  doc,
  index,
  page,
  width,
  onImage,
  onPick,
  editing,
  onEdit,
  onCancel,
  onReplace,
  busy,
  hint,
  onHintDone,
  dismissEditing,
}: {
  doc: PdfDoc;
  index: number;
  page: PdfPageInfo;
  width: number;
  onImage: (img: HTMLImageElement | null) => void;
  onPick: (page: number, area: Area, at: { x: number; y: number }) => void;
  editing: Editing | null;
  onEdit: (patch: Partial<Editing>) => void;
  onCancel: () => void;
  onReplace: () => void;
  busy: boolean;
  hint: { x: number; y: number; text: string } | null;
  onHintDone: () => void;
  dismissEditing: () => void;
}) {
  const scale = width / page.width;
  const height = page.height * scale;
  const box = useRef<HTMLDivElement>(null);
  const img = useRef<HTMLImageElement>(null);
  const [near, setNear] = useState(index < 2);
  const [src, setSrc] = useState<string | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  // The page being edited keeps its picture, wherever it is scrolled.
  const visible = near || editing != null;

  // Only pages near the screen hold a picture: one scrolled far away lets its picture go (and is
  // drawn again when it comes back), so a long PDF holds a few pages' pictures, not all it has
  // shown, and a zoom redraws only those.
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    // Measured against the PDF's own scroll area, so the margin reaches the pages it hides.
    const root = el.closest(".pdf-scroll");
    const o = new IntersectionObserver((entries) => entries.forEach((e) => setNear(e.isIntersecting)), {
      root,
      rootMargin: "1200px 0px",
    });
    o.observe(el);
    return () => o.disconnect();
  }, []);

  // The picture shown now, to let it go when it is replaced, dropped or the page unmounts.
  const shown = useRef<string | null>(null);
  const show = useCallback((u: string | null) => {
    const old = shown.current;
    shown.current = u;
    setSrc(u);
    // A moment later, so the <img> has moved on to the new picture first.
    if (old) window.setTimeout(() => URL.revokeObjectURL(old), 1000);
  }, []);
  useEffect(
    () => () => {
      if (shown.current) URL.revokeObjectURL(shown.current);
      shown.current = null;
    },
    [],
  );
  useEffect(() => {
    if (!visible) show(null);
  }, [visible, show]);

  // Drawn at the screen's own resolution, again after an edit or a zoom (a moment after it
  // settles). A drawing that comes back after the page moved on (scrolled away, zoomed again) is
  // let go at once.
  useEffect(() => {
    if (!visible) return;
    let alive = true;
    const pixels = Math.min(3200, Math.round(width * (window.devicePixelRatio || 1)));
    const t = window.setTimeout(() => {
      pdfRender(doc.id, index, pixels).then(
        (u) => {
          if (!alive) URL.revokeObjectURL(u);
          else show(u);
        },
        () => undefined,
      );
    }, shown.current ? 120 : 0);
    return () => {
      alive = false;
      window.clearTimeout(t);
    };
  }, [visible, doc.id, index, page.version, width, show]);

  useEffect(() => () => onImage(null), [onImage]);

  useEffect(() => {
    if (!hint) return;
    const t = window.setTimeout(onHintDone, 3500);
    return () => window.clearTimeout(t);
  }, [hint, onHintDone]);

  const local = (e: ReactPointerEvent) => {
    const r = box.current!.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };

  const down = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0 || busy) return;
    if (editing) {
      dismissEditing();
    }
    const p = local(e);
    e.currentTarget.setPointerCapture(e.pointerId);
    setDrag({ page: index, x0: p.x, y0: p.y, x1: p.x, y1: p.y });
  };
  const move = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!drag) return;
    const p = local(e);
    setDrag({ ...drag, x1: p.x, y1: p.y });
  };
  const up = () => {
    if (!drag) return;
    const d = drag;
    setDrag(null);
    const toPt = (x: number, y: number) => ({ x: x / scale, y: page.height - y / scale });
    if (Math.abs(d.x1 - d.x0) < 4 && Math.abs(d.y1 - d.y0) < 4) {
      const p = toPt(d.x0, d.y0);
      onPick(index, { kind: "point", x: p.x, y: p.y }, { x: d.x0, y: d.y0 });
    } else {
      const a = toPt(d.x0, d.y0);
      const b = toPt(d.x1, d.y1);
      onPick(
        index,
        { kind: "rect", rect: { left: Math.min(a.x, b.x), right: Math.max(a.x, b.x), bottom: Math.min(a.y, b.y), top: Math.max(a.y, b.y) } },
        { x: d.x1, y: d.y1 },
      );
    }
  };

  return (
    <div className="pdf-page" ref={box} style={{ width, height }}>
      {src ? (
        <img
          ref={(el) => {
            (img as { current: HTMLImageElement | null }).current = el;
            onImage(el);
          }}
          className="pdf-page__img"
          src={src}
          alt={`Page ${index + 1}`}
          draggable={false}
        />
      ) : (
        <div className="pdf-page__wait">
          <Spinner size={18} />
        </div>
      )}
      <div className="pdf-page__layer" onPointerDown={down} onPointerMove={move} onPointerUp={up} onPointerCancel={() => setDrag(null)}>
        {drag && (
          <div
            className="pdf-page__drag"
            style={{
              left: Math.min(drag.x0, drag.x1),
              top: Math.min(drag.y0, drag.y1),
              width: Math.abs(drag.x1 - drag.x0),
              height: Math.abs(drag.y1 - drag.y0),
            }}
          />
        )}
      </div>
      {hint && (
        <div className="pdf-page__hint caption" style={{ left: Math.min(hint.x, width - 260), top: hint.y + 14 }}>
          {hint.text}
        </div>
      )}
      {editing && <EditBox editing={editing} page={page} scale={scale} busy={busy} onEdit={onEdit} onCancel={onCancel} onReplace={onReplace} />}
      <div className="pdf-page__number caption">{index + 1}</div>
    </div>
  );
}

/** The picked text as an editing box in its own font, size and colour, over its place on the page. */
function EditBox({
  editing,
  page,
  scale,
  busy,
  onEdit,
  onCancel,
  onReplace,
}: {
  editing: Editing;
  page: PdfPageInfo;
  scale: number;
  busy: boolean;
  onEdit: (patch: Partial<Editing>) => void;
  onCancel: () => void;
  onReplace: () => void;
}) {
  const field = useRef<HTMLTextAreaElement>(null);
  const measure = useRef<HTMLSpanElement>(null);
  const { block } = editing;
  const lines = block.lines;
  const size = lines[0].size * scale;
  const step = lines.length > 1 ? ((lines[0].baseline - lines[lines.length - 1].baseline) / (lines.length - 1)) * scale : size * 1.25;
  const pad = Math.max(3, size * 0.15);
  const rect = block.rect;
  const left = rect.left * scale - pad;
  const top = (page.height - rect.top) * scale - pad;
  const minWidth = (rect.right - rect.left) * scale + pad * 2;
  const rows = Math.max(1, editing.text.split("\n").length);
  const [width, setWidth] = useState(minWidth);
  const f = block.font;
  const family = `"${f.family || "Arial"}", ${f.mono ? "monospace" : f.serif ? "serif" : "sans-serif"}`;
  const style: CSSProperties = {
    fontFamily: family,
    fontSize: size,
    lineHeight: `${step}px`,
    fontWeight: f.bold ? 700 : 400,
    fontStyle: f.italic ? "italic" : "normal",
    color: f.color,
    background: editing.background,
    textAlign: editing.align === "RIGHT" ? "right" : editing.align === "CENTER" ? "center" : "left",
    padding: pad,
    width,
    height: rows * step + pad * 2,
  };

  useEffect(() => {
    const el = field.current;
    if (!el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  }, []);

  // Grows with the longest line; a right-kept figure grows to the left.
  useLayoutEffect(() => {
    const m = measure.current;
    if (!m) return;
    const longest = Math.max(...editing.text.split("\n").map((l) => (m.textContent = l || " ", m.getBoundingClientRect().width)));
    setWidth(Math.max(minWidth, longest + pad * 2 + 2));
  }, [editing.text, minWidth, pad]);

  const shift = editing.align === "RIGHT" ? minWidth - width : editing.align === "CENTER" ? (minWidth - width) / 2 : 0;
  const below = top + rows * step + pad * 2 + 48 < page.height * scale;
  // Near the page's right edge the bar ends where the box ends, so it stays on the page.
  const barEnd = left + shift + (block.drawn ? 560 : 400) > page.width * scale;
  const face = `${f.family || f.name}${f.bold ? " Bold" : ""}${f.italic ? " Italic" : ""}`;
  return (
    <div className="pdf-edit" style={{ left: left + shift, top }} onPointerDown={(e) => e.stopPropagation()}>
      <span ref={measure} className="pdf-edit__measure" style={{ fontFamily: family, fontSize: size, fontWeight: style.fontWeight, fontStyle: style.fontStyle }} />
      <textarea
        ref={field}
        className="pdf-edit__field"
        style={style}
        value={editing.text}
        spellCheck={false}
        disabled={busy}
        onChange={(e) => onEdit({ text: e.target.value })}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            onCancel();
          } else if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            onReplace();
          }
        }}
      />
      <div
        className={["pdf-edit__bar", below ? "" : "pdf-edit__bar--above", barEnd ? "pdf-edit__bar--end" : ""].filter(Boolean).join(" ")}
      >
        <div className="nk-kind-switch pdf-align" role="tablist" aria-label="Keep this edge">
          {(["LEFT", "CENTER", "RIGHT"] as Align[]).map((a) => (
            <button
              key={a}
              type="button"
              role="tab"
              aria-selected={editing.align === a}
              title={a === "LEFT" ? "Keep the left edge" : a === "RIGHT" ? "Keep the right edge" : "Keep the middle"}
              className={editing.align === a ? "nk-kind-pill nk-kind-pill--selected" : "nk-kind-pill"}
              onClick={() => onEdit({ align: a })}
            >
              <AlignGlyph align={a} />
            </button>
          ))}
        </div>
        {block.drawn ? (
          // Text in a picture: set in the closest font, or made of the page's own letters.
          <div className="nk-kind-switch pdf-letters" role="tablist" aria-label="Letters">
            <button
              type="button"
              role="tab"
              aria-selected={!editing.fromPage}
              title={`Set as text in ${face}, the closest font on this computer (${Math.round(lines[0].size * 10) / 10} pt): sharp, and it can be changed again`}
              className={!editing.fromPage ? "nk-kind-pill nk-kind-pill--selected" : "nk-kind-pill"}
              onClick={() => onEdit({ fromPage: false })}
            >
              <span className="pdf-edit__font">{face}</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={editing.fromPage}
              title={`Made of letters cut from this page, so it looks like the scan itself; any the page does not have are drawn in ${face}`}
              className={editing.fromPage ? "nk-kind-pill nk-kind-pill--selected" : "nk-kind-pill"}
              onClick={() => onEdit({ fromPage: true })}
            >
              Page letters
            </button>
          </div>
        ) : (
          <span className="caption text-tertiary pdf-edit__font" title={f.name}>
            {face} · {Math.round(lines[0].size * 10) / 10} pt
          </span>
        )}
        <span className="nc-flex-spacer" />
        <TextLink text="Cancel" onClick={onCancel} />
        <Button text={busy ? "Putting it in…" : "Replace"} compact disabled={busy} onClick={onReplace} />
      </div>
    </div>
  );
}

function AlignGlyph({ align }: { align: Align }) {
  const x = (w: number) => (align === "LEFT" ? 2 : align === "RIGHT" ? 14 - w : (16 - w) / 2);
  return (
    <svg width="14" height="12" viewBox="0 0 16 12" aria-hidden>
      {[10, 14, 8].map((w, i) => (
        <rect key={i} x={x(w)} y={1 + i * 4} width={w} height="2" rx="1" fill="currentColor" />
      ))}
    </svg>
  );
}

/**
 * The page's colour around the picked text: the most common of a ring of pixels just outside it,
 * so the editing box hides the old words on a tinted panel as on white paper.
 */
function backgroundAround(img: HTMLImageElement | undefined, page: PdfPageInfo, block: Block): string {
  if (!img || !img.complete || img.naturalWidth === 0) return "#ffffff";
  try {
    const k = img.naturalWidth / page.width;
    const r = block.rect;
    const x0 = Math.max(0, Math.floor(r.left * k) - 3);
    const x1 = Math.min(img.naturalWidth - 1, Math.ceil(r.right * k) + 3);
    const y0 = Math.max(0, Math.floor((page.height - r.top) * k) - 3);
    const y1 = Math.min(img.naturalHeight - 1, Math.ceil((page.height - r.bottom) * k) + 3);
    const canvas = document.createElement("canvas");
    canvas.width = x1 - x0 + 1;
    canvas.height = y1 - y0 + 1;
    const ctx = canvas.getContext("2d", { willReadFrequently: true });
    if (!ctx) return "#ffffff";
    ctx.drawImage(img, x0, y0, canvas.width, canvas.height, 0, 0, canvas.width, canvas.height);
    const data = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
    const count = new Map<string, number>();
    const at = (x: number, y: number) => {
      const i = (y * canvas.width + x) * 4;
      const key = `${data[i]},${data[i + 1]},${data[i + 2]}`;
      count.set(key, (count.get(key) ?? 0) + 1);
    };
    for (let x = 0; x < canvas.width; x += 2) {
      at(x, 0);
      at(x, canvas.height - 1);
    }
    for (let y = 0; y < canvas.height; y += 2) {
      at(0, y);
      at(canvas.width - 1, y);
    }
    const [best] = [...count.entries()].sort((a, b) => b[1] - a[1])[0] ?? ["255,255,255"];
    return `rgb(${best})`;
  } catch {
    return "#ffffff";
  }
}
