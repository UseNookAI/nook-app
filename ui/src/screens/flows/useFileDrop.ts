/**
 * MediaPicker.kt: files dragged in from Explorer, and Windows' own Open dialog.
 *
 * In the app the window hands over dropped files' paths (Tauri's drag-and-drop event: the page
 * never sees the file itself); in the browser preview an HTML drop gives only a name, which the
 * mocks take as a path.
 */
import { useEffect, useRef, useState } from "react";
import { inTauri } from "../../api/ipc";
import { MEDIA_EXTENSIONS } from "../../api/flows";

/**
 * Calls `onFile` with the first file dropped anywhere on the window while `enabled`; `hover` says
 * whether files are being dragged over it now.
 */
export function useFileDrop(enabled: boolean, onFile: (path: string) => void): { hover: boolean } {
  const [hover, setHover] = useState(false);
  const latest = useRef(onFile);
  latest.current = onFile;

  useEffect(() => {
    if (!enabled) {
      setHover(false);
      return;
    }
    if (inTauri) {
      let unlisten: (() => void) | null = null;
      let cancelled = false;
      import("@tauri-apps/api/webview").then(({ getCurrentWebview }) =>
        getCurrentWebview()
          .onDragDropEvent((e) => {
            const p = e.payload;
            if (p.type === "enter" || p.type === "over") setHover(true);
            else if (p.type === "leave") setHover(false);
            else if (p.type === "drop") {
              setHover(false);
              if (p.paths.length > 0) latest.current(p.paths[0]);
            }
          })
          .then((fn) => {
            if (cancelled) fn();
            else unlisten = fn;
          }),
      );
      return () => {
        cancelled = true;
        unlisten?.();
        setHover(false);
      };
    }
    const over = (e: DragEvent) => {
      if (!e.dataTransfer?.types.includes("Files")) return;
      e.preventDefault();
      setHover(true);
    };
    const leave = (e: DragEvent) => {
      if (e.relatedTarget == null) setHover(false);
    };
    const drop = (e: DragEvent) => {
      e.preventDefault();
      setHover(false);
      const file = e.dataTransfer?.files[0];
      if (file) latest.current(`C:\\Users\\you\\Downloads\\${file.name}`);
    };
    window.addEventListener("dragover", over);
    window.addEventListener("dragleave", leave);
    window.addEventListener("drop", drop);
    return () => {
      window.removeEventListener("dragover", over);
      window.removeEventListener("dragleave", leave);
      window.removeEventListener("drop", drop);
    };
  }, [enabled]);

  return { hover };
}

const TITLE = "Choose an audio or video file";

/** Windows' Open dialog, filtered to the types Nook reads; null when nothing was chosen. */
export async function chooseMediaFile(start: string | null): Promise<string | null> {
  if (inTauri) {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({
      title: TITLE,
      multiple: false,
      directory: false,
      defaultPath: start ?? undefined,
      filters: [
        { name: "Audio and video", extensions: MEDIA_EXTENSIONS },
        { name: "All files", extensions: ["*"] },
      ],
    });
    return typeof picked === "string" ? picked : null;
  }
  const typed = window.prompt(TITLE, "C:\\Users\\you\\Downloads\\interview.mp3");
  return typed && typed.trim() ? typed.trim() : null;
}
