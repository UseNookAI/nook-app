/**
 * The area picker: a see-through window over one screen (one over each), dimmed, where the person
 * drags out what to record. The rectangle stays lit with its size in the screen's own pixels;
 * Enter or "Record this area" sends it back to Nook, Escape or "Cancel" closes every picker.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { captureAreaPicked } from "../../api/capture";
import "./capture.css";

export interface PickAreaView {
  view: "pick-area";
  /** The screen's handle. */
  screen: number;
  /** Its size in its own pixels. */
  width: number;
  height: number;
}

interface Box {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

/** The smallest area, in the screen's pixels. */
const SMALLEST = 16;

export function AreaPicker({ view }: { view: PickAreaView }) {
  const [box, setBox] = useState<Box | null>(null);
  const [dragging, setDragging] = useState(false);
  const sent = useRef(false);

  useEffect(() => {
    document.documentElement.classList.add("ap-page");
    return () => document.documentElement.classList.remove("ap-page");
  }, []);

  /** CSS pixels to the screen's own. */
  const ratio = view.width / Math.max(1, window.innerWidth);
  const rect = box
    ? {
        left: Math.min(box.x0, box.x1),
        top: Math.min(box.y0, box.y1),
        width: Math.abs(box.x1 - box.x0),
        height: Math.abs(box.y1 - box.y0),
      }
    : null;
  const px = rect
    ? {
        x: Math.max(0, Math.round(rect.left * ratio)),
        y: Math.max(0, Math.round(rect.top * ratio)),
        width: Math.round(rect.width * ratio),
        height: Math.round(rect.height * ratio),
      }
    : null;
  const usable = !!px && px.width >= SMALLEST && px.height >= SMALLEST;

  const answer = useCallback(
    (use: boolean) => {
      if (sent.current) return;
      sent.current = true;
      if (use && px && usable) {
        const width = Math.min(px.width, view.width - px.x);
        const height = Math.min(px.height, view.height - px.y);
        void captureAreaPicked({ kind: "area", screen: view.screen, x: px.x, y: px.y, width, height });
      } else {
        void captureAreaPicked(null);
      }
    },
    [px, usable, view],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") answer(false);
      if (e.key === "Enter" && usable) answer(true);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [answer, usable]);

  return (
    <div
      className={rect ? "ap" : "ap ap--dim"}
      onPointerDown={(e) => {
        if (e.button !== 0 || (e.target as HTMLElement).closest("button")) return;
        (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
        setDragging(true);
        setBox({ x0: e.clientX, y0: e.clientY, x1: e.clientX, y1: e.clientY });
      }}
      onPointerMove={(e) => {
        if (dragging) setBox((b) => (b ? { ...b, x1: e.clientX, y1: e.clientY } : b));
      }}
      onPointerUp={() => setDragging(false)}
    >
      {!rect && (
        <div className="ap-hint">
          <span className="subtitle2">Drag over what to record</span>
          <span className="caption">Then press Enter · Esc to cancel</span>
        </div>
      )}
      {rect && (
        <div className="ap-box" style={{ left: rect.left, top: rect.top, width: rect.width, height: rect.height }}>
          <span className="ap-size caption">{px ? `${px.width} × ${px.height}` : ""}</span>
          {!dragging && (
            <div className={rect.top + rect.height + 52 > window.innerHeight ? "ap-actions ap-actions--inside" : "ap-actions"}>
              <button type="button" className="ap-button ap-button--main" disabled={!usable} onClick={() => answer(true)}>
                Record this area
              </button>
              <button type="button" className="ap-button" onClick={() => setBox(null)}>
                Choose again
              </button>
              <button type="button" className="ap-button" onClick={() => answer(false)}>
                Cancel
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
