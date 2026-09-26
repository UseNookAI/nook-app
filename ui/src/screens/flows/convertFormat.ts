/** The converter's wording and choices, kept pure for the tests. */
import type { FileInfo, Job, Kind, Need, Offer, Target } from "../../api/convert";
import { bytesText } from "./format";

/** The picker's groups, in its order. */
export const KINDS: { kind: Kind; label: string; icon: string }[] = [
  { kind: "PDF", label: "PDF", icon: "text" },
  { kind: "DOCUMENT", label: "Documents", icon: "text" },
  { kind: "SHEET", label: "Sheets", icon: "dashboard" },
  { kind: "SLIDES", label: "Slides", icon: "screen" },
  { kind: "WEB", label: "Web", icon: "text" },
  { kind: "TEXT", label: "Text", icon: "text" },
  { kind: "EBOOK", label: "E-books", icon: "text" },
  { kind: "IMAGE", label: "Pictures", icon: "picture" },
];

/** The icon for a file of `kind`; a warning for one Nook does not read. */
export function kindIcon(kind: Kind | null): string {
  return kind ? (KINDS.find((k) => k.kind === kind)?.icon ?? "text") : "alert-circle";
}

/** The targets in the picker's groups, empty groups left out. */
export function groupTargets(targets: Target[]): { kind: Kind; label: string; targets: Target[] }[] {
  return KINDS.map((k) => ({ kind: k.kind, label: k.label, targets: targets.filter((t) => t.kind === k.kind) })).filter(
    (g) => g.targets.length > 0,
  );
}

/**
 * The target to show chosen: the one chosen before while it is offered, else the one the
 * request asked for ("to PDF"), else none.
 */
export function pickTarget(offer: Offer | null, chosen: string | null, asked: string | null): string | null {
  const offered = (id: string | null) => id != null && offer?.targets.some((t) => t.id === id);
  if (offered(chosen)) return chosen;
  if (offered(asked)) return asked;
  return null;
}

/** "One-time download: the document engine (Pandoc)." */
export function needsText(needs: Need[]): string {
  const named = needs.map((n) => n.what);
  const list = named.length > 1 ? `${named.slice(0, -1).join(", ")} and ${named[named.length - 1]}` : named[0];
  return `One-time download: ${list}. After that it converts without the internet.`;
}

export const needsBytes = (needs: Need[]) => needs.reduce((sum, n) => sum + n.bytes, 0);

/** "report.docx", "3 files", "3 pictures". */
export function filesText(files: FileInfo[]): string {
  if (files.length === 1) return files[0].name;
  const pictures = files.every((f) => f.kind === "IMAGE");
  return `${files.length} ${pictures ? "pictures" : "files"}`;
}

/** The line above the Convert button. */
export function goText(files: FileInfo[], target: Target | null, combine: boolean): string {
  if (!target) return files.length > 0 ? "Pick what they should become." : "";
  if (combine && target.id === "pdf" && files.length > 1) return `${files.length} pictures into one PDF · by ${target.by}`;
  return `${filesText(files)} into ${target.name} · by ${target.by}`;
}

/** A job's title: "report.docx into PDF", "3 files into Markdown". */
export function jobTitle(job: Job): string {
  const what = job.items.length === 1 ? job.items[0].name : `${job.items.length} files`;
  return `${what} into ${job.toName}`;
}

/** Where a job stands, under its title. */
export function jobStatusText(job: Job): string {
  const done = job.items.filter((i) => i.status === "DONE").length;
  const failed = job.items.filter((i) => i.status === "FAILED").length;
  const n = job.items.length;
  switch (job.status) {
    case "WAITING":
      return "Waiting for the conversion before it";
    case "CONVERTING":
      return n > 1 ? `Converting · ${done + failed} of ${n}` : "Converting";
    case "STOPPED":
      return done > 0 ? `Stopped · ${done} of ${n} converted` : "Stopped";
    case "FAILED":
      return n > 1 ? `${failed} of ${n} could not be converted` : "Could not be converted";
    default:
      return n > 1 ? `${n} converted` : "Converted";
  }
}

/** The name of a result from its path. */
export const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/** "12 MB" for a download's size. */
export { bytesText };
