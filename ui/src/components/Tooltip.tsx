/**
 * A hover hint (Compose TooltipArea as ContextMeter and ToolbarIcon used it): a small raised card
 * with caption text, shown after `delay` ms above the element ("top", 6 px off) or below it
 * ("bottom", 4 px off), centred on it and kept inside the window. `roomy` gives a hint of several
 * lines 8 px above and below its text instead of 4 (the context meter's).
 *
 *   <Tooltip text="Copy"><IconButton icon="copy" ... /></Tooltip>
 */
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import "./components.css";

export interface TooltipProps {
  text: ReactNode;
  children: ReactNode;
  placement?: "top" | "bottom";
  /** ms before it shows (TooltipArea: 300 to 400). */
  delay?: number;
  maxWidth?: number;
  /** Turns it off without unwrapping the child. */
  disabled?: boolean;
  /** 8 px above and below the text instead of 4, for a hint of several lines. */
  roomy?: boolean;
}

export function Tooltip({ text, children, placement = "top", delay = 400, maxWidth = 340, disabled = false, roomy = false }: TooltipProps) {
  const anchor = useRef<HTMLSpanElement>(null);
  const tip = useRef<HTMLDivElement>(null);
  const timer = useRef<number | undefined>(undefined);
  const [shown, setShown] = useState(false);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  useEffect(() => () => window.clearTimeout(timer.current), []);
  useEffect(() => {
    if (disabled) setShown(false);
  }, [disabled]);

  useLayoutEffect(() => {
    if (!shown || !anchor.current || !tip.current) {
      setPos(null);
      return;
    }
    const r = anchor.current.getBoundingClientRect();
    const w = tip.current.offsetWidth;
    const h = tip.current.offsetHeight;
    let top = placement === "top" ? r.top - 6 - h : r.bottom + 4;
    if (placement === "top" && top < 4) top = r.bottom + 4;
    const left = Math.max(4, Math.min(r.left + r.width / 2 - w / 2, window.innerWidth - w - 4));
    top = Math.max(4, Math.min(top, window.innerHeight - h - 4));
    setPos({ left, top });
  }, [shown, placement, text]);

  const enter = () => {
    if (disabled) return;
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setShown(true), delay);
  };
  const leave = () => {
    window.clearTimeout(timer.current);
    setShown(false);
  };

  return (
    <>
      <span ref={anchor} className="nk-tooltip-anchor" onMouseEnter={enter} onMouseLeave={leave} onMouseDown={leave}>
        {children}
      </span>
      {shown &&
        createPortal(
          <div
            ref={tip}
            role="tooltip"
            className={roomy ? "nk-tooltip nk-tooltip--roomy" : "nk-tooltip"}
            style={{ maxWidth, left: pos?.left ?? 0, top: pos?.top ?? 0, visibility: pos ? "visible" : "hidden" }}
          >
            {text}
          </div>,
          document.body,
        )}
    </>
  );
}
