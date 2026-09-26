/**
 * Browser stand-ins for the document converter. What files can become follows the app's routes
 * in short (Word, Excel and PowerPoint are "on the computer"); a job converts one file every
 * moment, and a file with "broken" in its name fails as a damaged one would.
 *
 * The document engine (Pandoc) starts not downloaded, so a text target shows its download;
 * `?convertMock=ready` starts with every engine in.
 */
import type { Engine, FileInfo, Job, Kind, Need, Offer, Status, Target } from "../convert";
import type { Install } from "../flows";
import { mock, mockEmit } from "../ipc";

const F: [id: string, name: string, kind: Kind, also?: string[]][] = [
  ["pdf", "PDF", "PDF"],
  ["docx", "Word document", "DOCUMENT", ["docm", "dotx"]],
  ["doc", "Word 97-2003 document", "DOCUMENT"],
  ["odt", "OpenDocument text", "DOCUMENT"],
  ["rtf", "Rich Text", "DOCUMENT"],
  ["html", "Web page", "WEB", ["htm"]],
  ["md", "Markdown", "TEXT", ["markdown"]],
  ["txt", "Plain text", "TEXT"],
  ["tex", "LaTeX", "TEXT"],
  ["rst", "reStructuredText", "TEXT"],
  ["adoc", "AsciiDoc", "TEXT"],
  ["ipynb", "Jupyter notebook", "TEXT"],
  ["epub", "EPUB e-book", "EBOOK"],
  ["xlsx", "Excel workbook", "SHEET"],
  ["xls", "Excel 97-2003 workbook", "SHEET"],
  ["ods", "OpenDocument spreadsheet", "SHEET"],
  ["csv", "CSV", "SHEET"],
  ["tsv", "Tab-separated values", "SHEET"],
  ["json", "JSON rows", "SHEET"],
  ["pptx", "PowerPoint presentation", "SLIDES"],
  ["ppt", "PowerPoint 97-2003 presentation", "SLIDES"],
  ["odp", "OpenDocument presentation", "SLIDES"],
  ["png", "PNG picture", "IMAGE"],
  ["jpg", "JPEG picture", "IMAGE", ["jpeg"]],
  ["webp", "WebP picture", "IMAGE"],
  ["bmp", "Bitmap picture", "IMAGE"],
  ["tiff", "TIFF picture", "IMAGE", ["tif"]],
  ["gif", "GIF picture", "IMAGE"],
  ["ico", "Windows icon", "IMAGE"],
];

const format = (id: string) => F.find((f) => f[0] === id);
const ofPath = (path: string) => {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  return F.find((f) => f[0] === ext || f[3]?.includes(ext));
};

const PANDOC_WRITES = ["docx", "odt", "rtf", "html", "md", "txt", "tex", "rst", "adoc", "epub"];

/** Who converts `from` into `to` ("Word", "Pandoc + Edge"), or null when nothing does. */
function by(from: string, to: string): string | null {
  const a = format(from)!;
  const b = format(to)!;
  if (from === to) return null;
  switch (a[2]) {
    case "DOCUMENT":
      if (to === "pdf" || b[2] === "DOCUMENT") return "Word";
      return PANDOC_WRITES.includes(to) ? "Pandoc" : null;
    case "TEXT":
    case "WEB":
    case "EBOOK":
      if (to === "pdf") return "Pandoc + Edge";
      if (to === "doc") return "Pandoc + Word";
      return PANDOC_WRITES.includes(to) ? "Pandoc" : null;
    case "PDF":
      if (b[2] === "IMAGE" && to !== "ico") return "PDFium";
      if (to === "txt") return "PDFium";
      return PANDOC_WRITES.includes(to) ? "PDFium + Pandoc" : null;
    case "SHEET":
      if (to === "pdf" || ["xlsx", "xls", "ods"].includes(to)) return ["csv", "tsv", "json"].includes(from) ? "Nook + Excel" : "Excel";
      return ["csv", "tsv", "json", "html", "md"].includes(to) ? "Nook" : null;
    case "SLIDES":
      return to === "pdf" || b[2] === "SLIDES" ? "PowerPoint" : null;
    case "IMAGE":
      if (to === "pdf") return "PDFium";
      return b[2] === "IMAGE" ? "Nook" : null;
  }
  return null;
}

const ENGINES: { engine: Engine; word: string; what: string; bytes: number }[] = [
  { engine: "PANDOC", word: "Pandoc", what: "the document engine (Pandoc)", bytes: 41_761_100 },
  { engine: "PDF", word: "PDFium", what: "the PDF engine", bytes: 3_733_154 },
  { engine: "LIBRE_OFFICE", word: "LibreOffice", what: "the office engine (LibreOffice)", bytes: 373_252_096 },
];
const ready = new URLSearchParams(window.location.search).get("convertMock")?.includes("ready");
const installed = new Set<Engine>(ready ? ["PANDOC", "PDF", "LIBRE_OFFICE"] : ["PDF"]);
let install: Install | null = null;
let installTimer: number | undefined;

function offer(paths: string[]): Offer {
  const files: FileInfo[] = paths.map((path) => {
    const f = ofPath(path);
    return { path, name: path.split(/[\\/]/).pop() ?? path, format: f?.[0] ?? null, formatName: f?.[1] ?? null, kind: f?.[2] ?? null };
  });
  const known = files.filter((f) => f.format).map((f) => f.format!);
  const targets: Target[] = [];
  if (known.length > 0) {
    for (const [id, name, kind] of F) {
      const bys = known.map((from) => by(from, id));
      if (bys.some((b) => b == null)) continue;
      const needs: Need[] = ENGINES.filter((e) => !installed.has(e.engine) && bys.some((b) => b!.includes(e.word))).map((e) => ({
        engine: e.engine,
        what: e.what,
        bytes: e.bytes,
      }));
      targets.push({ id, name, kind, by: [...new Set(bys.flatMap((b) => b!.split(" + ")))].join(" + "), needs, missing: null });
    }
  }
  const note =
    paths.length === 0
      ? null
      : known.length === 0
        ? "Nook does not read these files."
        : targets.length === 0
          ? "These files have no format in common to become: convert them one kind at a time."
          : null;
  const combine = known.length > 1 && known.length === paths.length && files.every((f) => f.kind === "IMAGE");
  return { files, targets, combine, note };
}

let jobs: Job[] = [];
let nextJob = 1;
const timers = new Map<string, number>();

function emit(job: Job) {
  jobs = jobs.map((j) => (j.id === job.id ? job : j));
  mockEmit("convert", { job });
}

function run(id: string, folder: string | null) {
  const timer = window.setInterval(() => {
    const job = jobs.find((j) => j.id === id);
    if (!job) return;
    const i = job.items.findIndex((it) => it.status === "WAITING" || it.status === "CONVERTING");
    if (i < 0) {
      window.clearInterval(timer);
      const status: Status = job.items.some((it) => it.status === "FAILED") ? "FAILED" : "DONE";
      emit({ ...job, status });
      return;
    }
    const items = [...job.items];
    const item = items[i];
    if (item.status === "WAITING") items[i] = { ...item, status: "CONVERTING" };
    else if (/broken/i.test(item.name)) items[i] = { ...item, status: "FAILED", error: "The file is damaged, or not what its name says." };
    else {
      const stem = item.name.replace(/\.[^.]+$/, "");
      const dir = folder ?? item.input.replace(/[\\/][^\\/]*$/, "");
      const pages = job.to === "png" && /\.pdf$/i.test(item.name);
      const outputs = pages
        ? [1, 2, 3].map((n) => `${dir}\\${stem} pages\\${stem} ${n}.png`)
        : [`${dir}\\${stem}.${job.to}`];
      items[i] = { ...item, status: "DONE", outputs };
    }
    emit({ ...job, status: "CONVERTING", items });
  }, 600);
  timers.set(id, timer);
}

export function registerConvertMocks(): void {
  mock("convert_offer", (a) => offer(a.paths as string[]));
  mock("convert_install_state", () => install);
  mock("convert_install", (a) => {
    if (install && !install.error) return;
    const wanted = ENGINES.filter((e) => (a.engines as Engine[]).includes(e.engine) && !installed.has(e.engine));
    if (wanted.length === 0) return;
    const total = wanted.reduce((s, e) => s + e.bytes, 0);
    install = { what: wanted.map((e) => e.what).join(" and "), done: 0, total, error: null };
    mockEmit("convert", { install });
    installTimer = window.setInterval(() => {
      const done = Math.min(total, (install?.done ?? 0) + total / 25);
      if (done >= total) {
        window.clearInterval(installTimer);
        wanted.forEach((e) => installed.add(e.engine));
        install = null;
      } else install = { ...install!, done };
      mockEmit("convert", { install });
    }, 120);
  });
  mock("convert_cancel_install", () => {
    window.clearInterval(installTimer);
    if (install) install = { ...install, error: "The download was stopped." };
    mockEmit("convert", { install });
  });
  mock("convert_clear_install_error", () => {
    if (install?.error) install = null;
    mockEmit("convert", { install });
  });
  mock("convert_start", (a) => {
    const paths = a.paths as string[];
    const to = a.to as string;
    const combine = a.combine as boolean;
    const target = offer(paths).targets.find((t) => t.id === to);
    if (!target) throw new Error(`These files cannot become ${format(to)?.[1] ?? to}.`);
    if (target.needs.length > 0) throw new Error(`Download ${target.needs.map((n) => n.what).join(" and ")} first.`);
    const inputs = combine ? [paths[0]] : paths;
    const job: Job = {
      id: `convert_${nextJob++}`,
      to,
      toName: target.name,
      at: Date.now(),
      status: "WAITING",
      items: inputs.map((input) => ({
        input,
        name: combine ? `${paths.length} pictures` : (input.split(/[\\/]/).pop() ?? input),
        status: "WAITING",
        outputs: [],
        error: null,
      })),
    };
    jobs = [job, ...jobs];
    mockEmit("convert", { job });
    run(job.id, (a.folder as string | null) ?? null);
    return job;
  });
  mock("convert_jobs", () => jobs);
  mock("convert_cancel", (a) => {
    const job = jobs.find((j) => j.id === a.id);
    if (!job) return;
    window.clearInterval(timers.get(job.id));
    const items = job.items.map((it) => (it.status === "WAITING" || it.status === "CONVERTING" ? { ...it, status: "STOPPED" as Status } : it));
    emit({ ...job, status: "STOPPED", items });
  });
  mock("convert_open", () => undefined);
  mock("convert_reveal", () => undefined);
}
