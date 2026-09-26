/**
 * The page sheet every main view sits on (PagePane.kt): the page colour, rounded, with a hairline
 * all round, lying on the canvas a little in from the window's right and bottom edges. Content is
 * clipped to the sheet, so it scrolls inside it, and it is `position: relative` so overlays can
 * align to it. It fills its parent (a flex column or a sized box).
 */
import type { CSSProperties, ReactNode } from "react";
import "./hub.css";

export function PagePane({ children, className, style }: { children?: ReactNode; className?: string; style?: CSSProperties }) {
  return (
    <div className={["nk-page-pane", className ?? ""].filter(Boolean).join(" ")} style={style}>
      {children}
    </div>
  );
}
