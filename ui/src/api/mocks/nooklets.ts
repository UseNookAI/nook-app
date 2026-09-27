/**
 * Browser stand-ins for the Nooklets' finder. The model is played by word lists (as the app does
 * while the finder is not downloaded), with a short pause so the critter can be seen searching.
 *
 * `?nookletsMock=fresh` starts with the finder not downloaded yet.
 */
import type { Install } from "../flows";
import { mock, mockEmit } from "../ipc";
import type { Found, Hit, NookletId, Preset } from "../nooklets";

const FINDER_BYTES = 132_439_008 + 31_457_280;
let installed = !new URLSearchParams(window.location.search).get("nookletsMock")?.includes("fresh");
let install: Install | null = null;
let installTimer: number | undefined;

const NOOKLETS: { id: NookletId; title: string; blurb: string; words: string[] }[] = [
  {
    id: "translate",
    title: "Translate speech",
    blurb: "Speak or drop a file; hear it in another language",
    words: ["translate", "speech", "speak", "say", "voice", "audio", "video", "podcast", "dub", "language", "çevir", "übersetze", "traduire", "traducir"],
  },
  {
    id: "transcribe",
    title: "Transcribe a recording",
    blurb: "A meeting, a lecture, an interview or a voice memo, written down, with notes.",
    words: ["transcri", "recording", "meeting", "memo", "lecture", "interview", "notes", "minutes", "caption", "yazıya", "transkrib"],
  },
  {
    id: "summarize",
    title: "Summarize a document",
    blurb: "The key points of a long PDF, report, contract or article, and what to watch for.",
    words: ["summar", "summaries", "tldr", "gist", "points", "overview", "contract", "report", "özet", "zusammen", "résum", "resum"],
  },
  {
    id: "read-aloud",
    title: "Read it aloud",
    blurb: "Any document or text, read to you by a natural voice: an audiobook, a voiceover.",
    words: ["aloud", "loud", "listen", "audiobook", "narrat", "voiceover", "sesli", "vorlesen", "vor"],
  },
  {
    id: "pdf",
    title: "Edit a PDF",
    blurb: "Change any text; the font stays the same",
    words: ["edit", "change", "fix", "typo", "replace", "correct", "scan", "form", "invoice", "contract", "değiştir", "ändern", "modifier"],
  },
  {
    id: "convert",
    title: "Convert documents",
    blurb: "Any document, sheet, slide or picture into another format",
    words: ["convert", "turn", "into", "make", "save", "export", "word", "excel", "powerpoint", "docx", "csv", "jpg", "png", "photos", "markdown", "dönüştür", "umwandeln", "convertir"],
  },
];

const LANGUAGES: [RegExp, string, string][] = [
  [/\b(german|deutsch|almanca|allemand|alemán)/i, "de", "German"],
  [/\b(english|englisch|ingilizce|anglais|inglés)/i, "en", "English"],
  [/\b(french|französisch|fransızca|français|francés)/i, "fr", "French"],
  [/\b(spanish|spanisch|ispanyolca|español|espagnol)/i, "es", "Spanish"],
  [/\b(italian|italienisch|italyanca|italiano)/i, "it", "Italian"],
  [/\b(turkish|türkisch|türkçe|turc)/i, "tr", "Turkish"],
];

const FORMATS: [RegExp, string, string][] = [
  [/\bpdf\b/i, "pdf", "PDF"],
  [/\b(word|docx)\b/i, "docx", "Word document"],
  [/\b(excel|xlsx)\b/i, "xlsx", "Excel workbook"],
  [/\bcsv\b/i, "csv", "CSV"],
  [/\b(powerpoint|pptx)\b/i, "pptx", "PowerPoint presentation"],
  [/\bmarkdown\b|\bmd\b/i, "md", "Markdown"],
  [/\b(html|web page)\b/i, "html", "Web page"],
  [/\bepub\b/i, "epub", "EPUB e-book"],
  [/\bpng\b/i, "png", "PNG picture"],
  [/\b(jpg|jpeg)\b/i, "jpg", "JPEG picture"],
];

/** The language or format named last, after "into"/"to" when there is one (catalog.rs preset). */
function preset(id: NookletId, request: string): Preset | null {
  const table = id === "convert" ? FORMATS : id === "pdf" ? null : LANGUAGES;
  if (!table) return null;
  const after = request.match(/\b(?:into|to|as|in|ins|en|zu)\b(.*)$/i)?.[1] ?? "";
  let best: { at: number; hit: [RegExp, string, string] } | null = null;
  for (const text of [after, request]) {
    for (const row of table) {
      const m = row[0].exec(text);
      if (m && (!best || m.index >= best.at)) best = { at: m.index, hit: row };
    }
    if (best) break;
  }
  if (!best) return null;
  const [, value, name] = best.hit;
  if (id === "convert") return { key: "format", value, label: `to ${name}` };
  return { key: "language", value, label: id === "translate" ? `into ${name}` : `in ${name}` };
}

function find(request: string): Found {
  const words = request.toLowerCase().split(/[^\p{L}\p{N}']+/u).filter(Boolean);
  const hits: Hit[] = NOOKLETS.map((n) => {
    const count = words.filter((w) => n.words.some((k) => w.startsWith(k))).length;
    const score = Math.min(0.99, count === 0 ? 0.2 : 0.8 + 0.07 * count);
    return { id: n.id, title: n.title, blurb: n.blurb, score, fits: score >= 0.85, preset: preset(n.id, request) };
  }).sort((a, b) => b.score - a.score);
  const sure = hits[0].score >= 0.88 && hits[0].score - hits[1].score > 0.05;
  return { hits, sure, matched: hits[0].score >= 0.85, by: installed ? "model" : "words" };
}

export function registerNookletsMocks(): void {
  mock("nooklets_setup", () => ({ installed, bytes: installed ? 0 : FINDER_BYTES, install }));
  mock("nooklets_find", async (a) => {
    await new Promise((r) => setTimeout(r, installed ? 650 : 150));
    return find(a.request as string);
  });
  mock("nooklets_install", () => {
    if (installed || (install && !install.error)) return;
    install = { what: "the Nooklet finder", done: 0, total: FINDER_BYTES, error: null };
    mockEmit("nooklets", { install });
    installTimer = window.setInterval(() => {
      const done = Math.min(FINDER_BYTES, (install?.done ?? 0) + FINDER_BYTES / 30);
      if (done >= FINDER_BYTES) {
        window.clearInterval(installTimer);
        installed = true;
        install = null;
      } else install = { ...install!, done };
      mockEmit("nooklets", { install });
    }, 120);
  });
  mock("nooklets_cancel_install", () => {
    window.clearInterval(installTimer);
    if (install) install = { ...install, error: "The download was stopped." };
    mockEmit("nooklets", { install });
  });
  mock("nooklets_clear_install_error", () => {
    if (install?.error) install = null;
    mockEmit("nooklets", { install });
  });
}
