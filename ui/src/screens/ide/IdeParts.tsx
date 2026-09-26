/**
 * The Code page's own lines: the hairline between its bars and panes, and the one that drags to
 * size two panes (PaneDivider). Its other controls are the shared ones.
 */
import { useRef } from "react";

export function HairLine() {
  return <div className="ide-hairline" />;
}

/** The hairline between two panes, which drags to size them (ResizeHandle / PaneDivider). */
export function ResizeHandle({ onDrag, onDone }: { onDrag: (dx: number) => void; onDone: () => void }) {
  const last = useRef<number | null>(null);
  return (
    <div
      className="ide-resize"
      role="separator"
      aria-orientation="vertical"
      onPointerDown={(e) => {
        e.preventDefault();
        try {
          e.currentTarget.setPointerCapture(e.pointerId);
        } catch {
          // Not a live pointer (a synthetic event): the drag still follows the moves it gets.
        }
        last.current = e.clientX;
      }}
      onPointerMove={(e) => {
        if (last.current == null) return;
        const dx = e.clientX - last.current;
        last.current = e.clientX;
        if (dx !== 0) onDrag(dx);
      }}
      onPointerUp={(e) => {
        if (last.current == null) return;
        last.current = null;
        if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId);
        onDone();
      }}
      onPointerCancel={() => {
        if (last.current == null) return;
        last.current = null;
        onDone();
      }}
    />
  );
}
