/** Path wording the screens share: a path's last part, and a folder's name for pills and lists. */

const SEPARATORS = /[\\/]/;

/** The last part of a path, or the whole path when it has none (a drive root). */
export function baseName(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  if (/^[A-Za-z]:$/.test(trimmed)) return path;
  const parts = trimmed.split(SEPARATORS);
  return parts[parts.length - 1] || path;
}

/**
 * A folder's name for pills and lists (CodeSidebar.kt repoName: `Path.fileName`, or the path
 * itself when it has none): "C:\\src\\nook" is "nook", a drive root stays "C:\\", blank is "".
 */
export function repoName(path: string | null | undefined): string {
  if (!path || !path.trim()) return "";
  return baseName(path);
}
