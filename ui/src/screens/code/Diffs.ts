/** Diffs.kt: a unified diff by file, and the stat line under a reply. */

/** One file of a unified diff: its path, its lines, and how many were added and removed. */
export interface DiffFile {
  path: string;
  lines: string[];
  added: number;
  removed: number;
  isNew: boolean;
  isDeleted: boolean;
}

/** Splits `git diff` output into files; the header lines stay out of the body. */
export function parseDiff(diff: string): DiffFile[] {
  const files: DiffFile[] = [];
  let path: string | null = null;
  let body: string[] = [];
  let added = 0;
  let removed = 0;
  let isNew = false;
  let isDeleted = false;

  const flush = () => {
    if (path == null) return;
    files.push({ path, lines: body, added, removed, isNew, isDeleted });
  };
  for (const raw of diff.split("\n")) {
    const line = raw.endsWith("\r") ? raw.slice(0, -1) : raw;
    if (line.startsWith("diff --git ")) {
      flush();
      const rest = line.slice("diff --git ".length);
      const at = rest.indexOf(" b/");
      path = (at >= 0 ? rest.slice(at + 3) : rest).trim();
      body = [];
      added = 0;
      removed = 0;
      isNew = false;
      isDeleted = false;
      continue;
    }
    if (path == null) continue;
    if (line.startsWith("new file mode")) isNew = true;
    else if (line.startsWith("deleted file mode")) isDeleted = true;
    else if (
      line.startsWith("index ") ||
      line.startsWith("--- ") ||
      line.startsWith("+++ ") ||
      line.startsWith("similarity ") ||
      line.startsWith("rename ") ||
      line.startsWith("old mode") ||
      line.startsWith("new mode")
    ) {
      // header
    } else {
      if (line.startsWith("+")) added++;
      else if (line.startsWith("-")) removed++;
      body.push(line);
    }
  }
  flush();
  return files;
}

/** "3 files changed, 40 insertions(+), 2 deletions(-)" from `git diff --stat`, or a count from the files. */
export function statLine(stat: string | null, files: DiffFile[]): string {
  const lines = stat?.trim().split("\n");
  const last = lines?.[lines.length - 1]?.trim();
  if (last && last.includes("changed")) return last;
  const a = files.reduce((n, f) => n + f.added, 0);
  const r = files.reduce((n, f) => n + f.removed, 0);
  return `${files.length} ${files.length === 1 ? "file" : "files"} changed, +${a} −${r}`;
}
