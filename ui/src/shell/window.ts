/**
 * The undecorated Tauri window (NookWindowControls.kt and the sizing in NookAgentApplication.kt).
 * Everything is a no-op in a plain browser (`inTauri` false), so `npm run dev` works.
 */
import { useEffect, useState } from "react";
import { inTauri } from "../api/ipc";

type TauriWindow = import("@tauri-apps/api/window").Window;

let current: Promise<TauriWindow> | null = null;

/** The main window, or null outside Tauri. */
function win(): Promise<TauriWindow> | null {
  if (!inTauri) return null;
  current ??= import("@tauri-apps/api/window").then((m) => m.getCurrentWindow());
  return current;
}

async function run(action: (w: TauriWindow) => Promise<unknown>): Promise<void> {
  const w = win();
  if (!w) return;
  try {
    await action(await w);
  } catch (e) {
    console.warn("window call failed", e);
  }
}

export const minimizeWindow = () => run((w) => w.minimize());
export const toggleMaximizeWindow = () => run((w) => w.toggleMaximize());

let shown = false;

/**
 * The window's mode for a screen: the welcome and erase screens are a fixed 1200 x 800, the hub a
 * resizable 1280 x 720; either way centred on the screen. A maximised window is restored first.
 *
 * The window starts hidden (tauri.conf.json `"visible": false`) and the first call shows it once it
 * has its size, so it never jumps in view; it shows even when the sizing fails. (Should the page
 * never get here, the Rust side shows the window after a few seconds.)
 */
export function applyWindowMode(resizable: boolean, width: number, height: number): Promise<void> {
  return run(async (w) => {
    try {
      const { LogicalSize } = await import("@tauri-apps/api/dpi");
      if (await w.isMaximized()) await w.unmaximize();
      await w.setResizable(true);
      await w.setSize(new LogicalSize(width, height));
      await w.setResizable(resizable);
      await w.center();
    } finally {
      if (!shown) {
        shown = true;
        await w.show();
        await w.setFocus();
      }
    }
  });
}

/** Whether the window fills the screen (for the maximise / restore glyph). */
export function useIsMaximized(): boolean {
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    const w = win();
    if (!w) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    w.then(async (window) => {
      const read = () =>
        window
          .isMaximized()
          .then((m) => !cancelled && setMaximized(m))
          .catch(() => {});
      await read();
      const off = await window.onResized(read);
      if (cancelled) off();
      else unlisten = off;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
  return maximized;
}

/**
 * Routes every close of the window (Alt+F4, the taskbar) through `onClose` instead of closing at
 * once, so the shell can ask first when something is running. `onClose` decides and quits.
 */
export function useCloseRequest(onClose: () => void): void {
  useEffect(() => {
    const w = win();
    if (!w) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    w.then(async (window) => {
      const off = await window.onCloseRequested((e) => {
        e.preventDefault();
        onClose();
      });
      if (cancelled) off();
      else unlisten = off;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [onClose]);
}
