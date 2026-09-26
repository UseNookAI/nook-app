/**
 * Browser stand-ins for the PDF editor (PdfEditor over PDFium). The "PDF" is a three-page invoice
 * drawn on a canvas from a list of text items, so picking, replacing, undo and zoom all work in
 * the preview; a new letter that the item's text did not have is said to come from Windows, as a
 * font subset would make it. A stamp on the first page is part of a picture (a scanned slip): a
 * pick reads it after a moment and says which font it looks like, and once changed (in that font
 * or the page's own letters) it is text.
 *
 * `?pdfMock=fresh` starts with the PDF engine not downloaded yet.
 */
import type { Install } from "../flows";
import { mock, mockEmit } from "../ipc";
import type { Align, Area, Block, BlockLine, PdfDoc, Pick, Rect } from "../pdf";

interface Item {
  text: string;
  x: number;
  baseline: number;
  size: number;
  family: string;
  bold?: boolean;
  italic?: boolean;
  color: string;
  serif?: boolean;
  mono?: boolean;
  /** `x` is the right edge (a figure in a column). */
  right?: boolean;
  /** Part of a picture: the slip's paper colour, drawn behind it. */
  paper?: string;
  /** Still the picture's letters (not changed into text yet). */
  picture?: boolean;
}

const W = 612;
const H = 792;
let installed = !new URLSearchParams(window.location.search).get("pdfMock")?.includes("fresh");
const ENGINE_BYTES = 3_733_154;
let install: Install | null = null;
let installTimer: number | undefined;

function invoice(): Item[][] {
  const body = { size: 12, family: "Calibri", color: "#222222" };
  const mono = { size: 11, family: "Consolas", color: "#222222", mono: true, right: true };
  return [
    [
      { text: "Invoice 2026-0914", x: 64, baseline: 704, size: 26, family: "Georgia", bold: true, serif: true, color: "#1f4e79" },
      { text: "Issued 14 September 2026 · Due 14 October 2026", x: 64, baseline: 684, size: 10, family: "Calibri", color: "#666666" },
      { text: "Bill to:", x: 64, baseline: 658, ...body },
      { text: "Northwind Traders Ltd", x: 102, baseline: 658, ...body, bold: true },
      { text: ", 42 Harbour Road, Bristol BS1 4QA", x: 222, baseline: 658, ...body },
      { text: "Item", x: 72, baseline: 620, ...body, bold: true },
      { text: "Qty", x: 316, baseline: 620, ...body, bold: true },
      { text: "Amount", x: 540, baseline: 620, ...mono, bold: true },
      { text: "Website redesign", x: 72, baseline: 596, ...body },
      { text: "1", x: 316, baseline: 596, ...body },
      { text: "€4,200.00", x: 540, baseline: 596, ...mono },
      { text: "Hosting, 12 months", x: 72, baseline: 572, ...body },
      { text: "12", x: 316, baseline: 572, ...body },
      { text: "€360.00", x: 540, baseline: 572, ...mono },
      { text: "Support hours", x: 72, baseline: 548, ...body, italic: true },
      { text: "8", x: 316, baseline: 548, ...body },
      { text: "€640.00", x: 540, baseline: 548, ...mono },
      { text: "Total", x: 72, baseline: 524, ...body, bold: true },
      { text: "€5,200.00", x: 540, baseline: 524, ...mono, bold: true },
      { text: "Please pay by bank transfer to IBAN GB29 NWBK 6016 1331 9268 19 within 30 days.", x: 74, baseline: 488, ...body },
      { text: "RECEIVED 16/10/2026", x: 380, baseline: 430, size: 13, family: "Courier New", mono: true, color: "#2d3f8f", paper: "#fbf8f0", picture: true },
    ],
    [
      { text: "Terms and conditions", x: 64, baseline: 704, size: 26, family: "Georgia", bold: true, serif: true, color: "#1f4e79" },
      { text: "Payment is due within thirty days of the invoice date. Late payments may incur", x: 64, baseline: 668, ...body },
      { text: "interest at eight percent per year.", x: 64, baseline: 652, ...body },
      { text: "All work remains the property of the supplier until paid in full.", x: 64, baseline: 624, ...body, family: "Georgia", serif: true },
    ],
    [
      { text: "Thank you", x: 64, baseline: 704, size: 26, family: "Georgia", bold: true, serif: true, color: "#1f4e79" },
      { text: "We appreciate your business.", x: 64, baseline: 668, size: 18, family: "Calibri", color: "#c0392b" },
    ],
  ];
}

let pages: Item[][] = invoice();
let versions = [0, 0, 0];
let undo: Item[][][] = [];
let doc: PdfDoc | null = null;
const ctx = document.createElement("canvas").getContext("2d")!;

const font = (i: Item, px: number) => `${i.italic ? "italic " : ""}${i.bold ? "700 " : "400 "}${px}px "${i.family}", ${i.mono ? "monospace" : i.serif ? "serif" : "sans-serif"}`;

function bounds(i: Item): Rect {
  ctx.font = font(i, i.size);
  const w = ctx.measureText(i.text).width;
  const left = i.right ? i.x - w : i.x;
  return { left, right: left + w, bottom: i.baseline - i.size * 0.22, top: i.baseline + i.size * 0.75 };
}

const hits = (r: Rect, b: Rect) => Math.min(r.right, b.right) > Math.max(r.left, b.left) && Math.min(r.top, b.top) > Math.max(r.bottom, b.bottom);

function info(): PdfDoc {
  return { ...doc!, pages: pages.map((_, i) => ({ width: W, height: H, version: versions[i] })), canUndo: undo.length > 0 };
}

async function render(page: number, width: number): Promise<ArrayBuffer> {
  const k = width / W;
  const c = document.createElement("canvas");
  c.width = Math.round(W * k);
  c.height = Math.round(H * k);
  const g = c.getContext("2d")!;
  g.fillStyle = "#ffffff";
  g.fillRect(0, 0, c.width, c.height);
  const y = (pt: number) => (H - pt) * k;
  if (page === 0) {
    // the table and the note's tint
    g.fillStyle = "#eef3f8";
    g.fillRect(64 * k, y(632), 484 * k, 24 * k);
    g.fillRect(64 * k, y(536), 484 * k, 24 * k);
    g.strokeStyle = "#bbbbbb";
    g.lineWidth = Math.max(1, k * 0.75);
    for (const row of [632, 608, 584, 560, 536, 512]) {
      g.beginPath();
      g.moveTo(64 * k, y(row));
      g.lineTo(548 * k, y(row));
      g.stroke();
    }
    for (const col of [64, 306, 400, 548]) {
      g.beginPath();
      g.moveTo(col * k, y(632));
      g.lineTo(col * k, y(512));
      g.stroke();
    }
    g.fillStyle = "#fff3cd";
    g.fillRect(64 * k, y(502), 484 * k, 22 * k);
    g.fillStyle = "#e0a800";
    g.fillRect(64 * k, y(502), 3 * k, 22 * k);
  }
  // A scanned slip under a stamp: its paper stays when the stamp is changed.
  for (const i of pages[page].filter((i) => i.paper)) {
    g.fillStyle = i.paper!;
    g.fillRect(360 * k, y(i.baseline + 22), 200 * k, 36 * k);
  }
  for (const i of pages[page]) {
    g.font = font(i, i.size * k);
    g.fillStyle = i.color;
    g.textAlign = i.right ? "right" : "left";
    g.fillText(i.text, i.x * k, y(i.baseline));
  }
  await new Promise((r) => setTimeout(r, 60));
  const blob: Blob = await new Promise((r) => c.toBlob((b) => r(b!), "image/png"));
  return blob.arrayBuffer();
}

function pick(page: number, area: Area): Pick {
  const items = pages[page];
  let chosen: number[];
  if (area.kind === "point") {
    const at = items.findIndex((i) => {
      const b = bounds(i);
      return area.x >= b.left - 1 && area.x <= b.right + 1 && area.y >= b.bottom - 1 && area.y <= b.top + 1;
    });
    chosen = at < 0 ? [] : [at];
  } else {
    chosen = items.map((i, n) => (hits(area.rect, bounds(i)) ? n : -1)).filter((n) => n >= 0);
  }
  if (chosen.length === 0) return { block: null, why: "There is no text there. Drag across the words you want to change." };
  // Text in a picture is picked on its own, as the real reader finds it.
  const inPicture = chosen.filter((n) => items[n].picture);
  if (inPicture.length > 0) chosen = inPicture;
  chosen.sort((a, b) => items[b].baseline - items[a].baseline || bounds(items[a]).left - bounds(items[b]).left);
  const lines: BlockLine[] = [];
  for (const n of chosen) {
    const i = items[n];
    const b = bounds(i);
    const same = lines.find((l) => Math.abs(l.baseline - i.baseline) < i.size * 0.3);
    if (same) {
      same.objects.push(n);
      same.text = `${same.text} ${i.text}`.replace(/\s+,/g, ",");
      same.rect = { left: Math.min(same.rect.left, b.left), right: Math.max(same.rect.right, b.right), bottom: Math.min(same.rect.bottom, b.bottom), top: Math.max(same.rect.top, b.top) };
    } else lines.push({ objects: [n], text: i.text, rect: b, baseline: i.baseline, size: i.size });
  }
  const first = items[chosen[0]];
  const rect = lines.slice(1).reduce((r, l) => ({ left: Math.min(r.left, l.rect.left), right: Math.max(r.right, l.rect.right), bottom: Math.min(r.bottom, l.rect.bottom), top: Math.max(r.top, l.rect.top) }), lines[0].rect);
  const figure = lines.every((l) => /\d/.test(l.text) && /^[\d\s.,:;\-+/%()€$£]+$/.test(l.text));
  const block: Block = {
    page,
    version: versions[page],
    lines,
    rect,
    font: {
      name: first.picture ? `${first.family}${first.bold ? " Bold" : ""}` : `AAAAAA+${first.family}${first.bold ? "-Bold" : ""}`,
      family: first.family,
      bold: !!first.bold,
      italic: !!first.italic,
      serif: !!first.serif,
      mono: !!first.mono,
      color: first.color,
    },
    align: figure ? "RIGHT" : "LEFT",
    drawn: first.picture ? { paper: first.paper ?? "#ffffff", face: first.family.toLowerCase().replace(/[^a-z0-9]/g, ""), stretch: 1 } : null,
  };
  return { block, why: null };
}

function replace(block: Block, texts: string[], align: Align): { note: string | null } {
  if (versions[block.page] !== block.version) throw new Error("The page changed since that text was picked. Select it again.");
  undo.push(pages.map((p) => p.map((i) => ({ ...i }))));
  const items = pages[block.page];
  const gone = new Set<number>();
  let note: string | null = null;
  block.lines.forEach((line, n) => {
    const text = (texts[n] ?? "").trimEnd();
    const [keep, ...rest] = line.objects;
    rest.forEach((o) => gone.add(o));
    if (!text.trim()) {
      gone.add(keep);
      return;
    }
    const item = items[keep];
    const had = new Set(line.text);
    // The picture's letters are covered and the new text set over them: text from now on.
    if (item.picture) {
      item.picture = false;
      if (block.drawn?.fromPage) note = "Set in letters cut from this page.";
    }
    else if ([...text].some((c) => c.trim() && !had.has(c))) note = `The PDF carries only the letters it used from ${item.family}, so the changed text is set in ${item.family}${item.bold ? " Bold" : ""} from Windows.`;
    const old = bounds(item);
    const oldRight = Math.max(...line.objects.map((o) => bounds(items[o]).right));
    item.text = text;
    const now = bounds(item);
    if (align === "RIGHT") item.x += item.right ? oldRight - old.right : oldRight - now.right;
    if (align === "CENTER") item.x += (old.left + oldRight - now.left - now.right) / 2;
    if (align === "LEFT" && !item.right) {
      const grow = now.right - oldRight;
      items.forEach((o, i) => {
        if (i !== keep && !line.objects.includes(i) && Math.abs(o.baseline - item.baseline) < 3 && bounds(o).left >= oldRight - 1) o.x += grow;
      });
    }
  });
  const last = block.lines[block.lines.length - 1];
  const template = items[last.objects[0]];
  texts.slice(block.lines.length).forEach((t, n) => {
    if (t.trim()) items.push({ ...template, text: t.trimEnd(), baseline: last.baseline - (n + 1) * last.size * 1.3 });
  });
  pages[block.page] = items.filter((_, i) => !gone.has(i));
  versions[block.page] += 1;
  doc = { ...doc!, edits: doc!.edits + 1, dirty: true };
  return { note };
}

export function registerPdfMocks(): void {
  mock("pdf_setup", () => ({ installed, bytes: ENGINE_BYTES, install }));
  mock("pdf_install", () => {
    if (installed || (install && !install.error)) return;
    install = { what: "the PDF engine", done: 0, total: ENGINE_BYTES, error: null };
    mockEmit("pdf", { install });
    installTimer = window.setInterval(() => {
      const done = Math.min(ENGINE_BYTES, (install?.done ?? 0) + ENGINE_BYTES / 25);
      if (done >= ENGINE_BYTES) {
        window.clearInterval(installTimer);
        installed = true;
        install = null;
      } else install = { ...install!, done };
      mockEmit("pdf", { install });
    }, 120);
  });
  mock("pdf_cancel_install", () => {
    window.clearInterval(installTimer);
    if (install) install = { ...install, error: "The download was stopped." };
    mockEmit("pdf", { install });
  });
  mock("pdf_clear_install_error", () => {
    if (install?.error) install = null;
    mockEmit("pdf", { install });
  });
  mock("pdf_open", async (a) => {
    await new Promise((r) => setTimeout(r, 400));
    const path = a.path as string;
    pages = invoice();
    versions = [0, 0, 0];
    undo = [];
    doc = { id: "pdf_1", name: path.split(/[\\/]/).pop() ?? path, path, pages: [], edits: 0, canUndo: false, dirty: false, savedTo: null };
    return info();
  });
  mock("pdf_docs", () => (doc ? [info()] : []));
  mock("pdf_render", (a) => render(a.page as number, a.width as number));
  mock("pdf_pick", async (a) => {
    const p = pick(a.page as number, a.area as Area);
    // Reading a picture takes a moment.
    if (p.block?.drawn) await new Promise((r) => setTimeout(r, 700));
    return p;
  });
  mock("pdf_replace", (a) => {
    const { note } = replace(a.block as Block, a.texts as string[], a.align as Align);
    return { doc: info(), note };
  });
  mock("pdf_undo", () => {
    const back = undo.pop();
    if (!back) throw new Error("There is nothing to undo.");
    pages = back;
    versions = versions.map((v) => v + 1);
    doc = { ...doc!, edits: Math.max(0, doc!.edits - 1), dirty: true };
    return info();
  });
  mock("pdf_save", (a) => {
    const to = (a.path as string | null) ?? doc!.path.replace(/\.pdf$/i, " (edited).pdf");
    doc = { ...doc!, dirty: false, savedTo: to };
    return info();
  });
  mock("pdf_close", () => {
    doc = null;
  });
  mock("pdf_reveal", () => undefined);
}
