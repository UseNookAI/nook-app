/**
 * Which desktop the page is drawn on. On a Mac the window keeps macOS's own traffic lights over
 * the title strip (tauri.macos.conf.json), so Nook draws no minimise / maximise / close of its own
 * and leaves room at the strip's left; shortcuts say Cmd for Ctrl, and the file manager is the
 * Finder.
 *
 * In a plain browser `?platform=mac` or `?platform=windows` shows the other one.
 */
function detect(): "mac" | "windows" {
  if (typeof window === "undefined") return "windows";
  const asked = new URLSearchParams(window.location.search).get("platform");
  if (asked === "mac" || asked === "windows") return asked;
  return /Macintosh|Mac OS X/.test(window.navigator.userAgent) ? "mac" : "windows";
}

export const platform = detect();
export const isMac = platform === "mac";

/** The modifier key's name in a shortcut: "Cmd" on a Mac, "Ctrl" elsewhere. */
export const modKey = isMac ? "Cmd" : "Ctrl";

/** The file manager's name: "Finder" on a Mac, "Explorer" elsewhere. */
export const fileManager = isMac ? "Finder" : "Explorer";

/** What the GPU's memory is called: a card's VRAM, or on a Mac the share of its unified memory
 * the GPU may use. */
export const gpuMemory = isMac ? "GPU memory" : "VRAM";

/** Cmd on a Mac, Ctrl elsewhere: the key a shortcut is held with. */
export function withMod(e: { ctrlKey: boolean; metaKey: boolean }): boolean {
  return isMac ? e.metaKey : e.ctrlKey;
}
