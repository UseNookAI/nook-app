/**
 * FolderPicker.kt / CodeComposer.chooseFolder: Windows' own folder picker, the one Explorer uses.
 * The Kotlin app shipped a helper exe for it because the JVM only had Swing's chooser; Tauri's
 * dialog plugin shows the same system dialog, modal to Nook's window.
 *
 * Outside Tauri (the browser preview) a prompt stands in, so a path can still be typed.
 */
import { inTauri } from "../../api/ipc";

const TITLE = "Choose the folder Nook should work on";

/** The chosen folder, or null when the person cancelled. */
export async function chooseFolder(start: string | null): Promise<string | null> {
  if (inTauri) {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({ directory: true, multiple: false, title: TITLE, defaultPath: start ?? undefined });
    return typeof picked === "string" ? picked : null;
  }
  const typed = window.prompt(TITLE, start ?? "C:\\Users\\you\\Projects\\");
  return typed && typed.trim() ? typed.trim() : null;
}

/** Shows a folder in Explorer (the session header's folder action); nothing in the browser preview. */
export async function openFolder(path: string): Promise<void> {
  if (!inTauri) return;
  const { openPath } = await import("@tauri-apps/plugin-opener");
  await openPath(path);
}
