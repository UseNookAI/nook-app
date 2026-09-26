/** What the downloads strip beside the Models title lists (ModelDownloadsStrip.kt `downloadLines`). */

/** One download in flight, as the Models title shows it. */
export interface DownloadLine {
  name: string;
  progress: number;
  paused: boolean;
}

/** The part of a library model the strip needs (AiModelDto's id and name). */
export interface NamedModel {
  model: string;
  fullName: string | null;
}

/**
 * The downloads in flight, the library's first (the download service runs those), then the ones
 * the runtime runs itself: Hugging Face files, keyed "hub:<repo>:<file>", and catalog models an
 * agent asked for. A model both know of is listed once.
 */
export function downloadLines(
  library: readonly NamedModel[],
  downloading: readonly string[],
  paused: readonly string[],
  progress: Readonly<Record<string, number>>,
  runtime: Readonly<Record<string, number>>,
  catalogName: (id: string) => string | null,
): DownloadLine[] {
  const names = new Map(library.map((m) => [m.model, m.fullName ?? m.model]));
  const pausedSet = new Set(paused);
  // Kotlin's `downloading + paused`: a LinkedHashSet, each key once in first-seen order.
  const mine = [...new Set([...downloading, ...paused])];
  const mineSet = new Set(mine);
  const lines = mine.map((key) => ({ name: names.get(key) ?? key, progress: progress[key] ?? 0, paused: pausedSet.has(key) }));
  const theirs = Object.entries(runtime)
    .filter(([key]) => !mineSet.has(key))
    .map(([key, p]) => ({
      name: key.startsWith("hub:") ? key.slice(key.lastIndexOf(":") + 1) : (catalogName(key) ?? names.get(key) ?? key),
      progress: p,
      paused: false,
    }));
  return [...lines, ...theirs];
}
