/**
 * A surface floating beside an anchor element, like Compose's DropdownMenu: it opens below the
 * anchor (or above with placement "top-*"), flips when there is no room, stays inside the window,
 * and closes on a click outside it and its anchor, or on Escape.
 *
 * `DropdownMenu` is NookDropdownMenu (ComposerControls.kt): the raised surface with a hairline border
 * and the standard radius, to hold `StyledMenuItem`s.
 *
 *   const pill = useRef<HTMLButtonElement>(null);
 *   <ToolbarPill ref={pill} text="Model" expanded={open} onClick={() => setOpen(!open)} />
 *   <DropdownMenu anchor={pill} open={open} onClose={() => setOpen(false)}>
 *     <StyledMenuItem text="More models…" icon="download" onClick={...} />
 *   </DropdownMenu>
 */
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";
import "./components.css";

export type PopoverPlacement = "bottom-start" | "bottom-end" | "top-start" | "top-end";
export type PopoverAnchor = RefObject<HTMLElement | null> | HTMLElement | null;

export interface PopoverProps {
  anchor: PopoverAnchor;
  open: boolean;
  onClose: () => void;
  placement?: PopoverPlacement;
  /** Gap between the anchor and the surface, in px. */
  offset?: number;
  /** At least as wide as the anchor. */
  matchAnchorWidth?: boolean;
  className?: string;
  /** The surface's ARIA role ("menu" for a menu of StyledMenuItems). */
  role?: string;
  children: ReactNode;
}

const MARGIN = 8;

function element(anchor: PopoverAnchor): HTMLElement | null {
  if (!anchor) return null;
  return anchor instanceof HTMLElement ? anchor : anchor.current;
}

export function Popover({
  anchor,
  open,
  onClose,
  placement = "bottom-start",
  offset = 4,
  matchAnchorWidth = false,
  className,
  role,
  children,
}: PopoverProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number; minWidth?: number } | null>(null);

  const place = useCallback(() => {
    const a = element(anchor);
    const el = ref.current;
    if (!a || !el) return;
    const r = a.getBoundingClientRect();
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    const above = r.top - h - offset;
    const below = r.bottom + offset;
    let top = placement.startsWith("top") ? above : below;
    if (placement.startsWith("bottom") && below + h > window.innerHeight - MARGIN && above >= MARGIN) top = above;
    if (placement.startsWith("top") && above < MARGIN && below + h <= window.innerHeight - MARGIN) top = below;
    let left = placement.endsWith("end") ? r.right - w : r.left;
    left = Math.max(MARGIN, Math.min(left, window.innerWidth - w - MARGIN));
    top = Math.max(MARGIN, Math.min(top, window.innerHeight - h - MARGIN));
    setPos({ left, top, minWidth: matchAnchorWidth ? r.width : undefined });
  }, [anchor, placement, offset, matchAnchorWidth]);

  useLayoutEffect(() => {
    if (!open) {
      setPos(null);
      return;
    }
    place();
    const observer = new ResizeObserver(place);
    if (ref.current) observer.observe(ref.current);
    window.addEventListener("resize", place);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", place);
    };
  }, [open, place]);

  useEffect(() => {
    if (!open) return;
    const down = (e: MouseEvent) => {
      const target = e.target as Node;
      if (ref.current?.contains(target) || element(anchor)?.contains(target)) return;
      onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("mousedown", down);
    window.addEventListener("keydown", key, true);
    return () => {
      window.removeEventListener("mousedown", down);
      window.removeEventListener("keydown", key, true);
    };
  }, [open, anchor, onClose]);

  if (!open) return null;
  return createPortal(
    <div
      ref={ref}
      role={role}
      className={["nk-popover", className ?? ""].filter(Boolean).join(" ")}
      style={{
        left: pos?.left ?? 0,
        top: pos?.top ?? 0,
        minWidth: pos?.minWidth,
        visibility: pos ? "visible" : "hidden",
      }}
    >
      {children}
    </div>,
    document.body,
  );
}

/** NookDropdownMenu: a Popover on the raised surface, for StyledMenuItem rows. */
export function DropdownMenu({ className, ...props }: PopoverProps) {
  return <Popover {...props} className={["nk-dropdown", className ?? ""].filter(Boolean).join(" ")} />;
}
