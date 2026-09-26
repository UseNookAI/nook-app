/**
 * A floating menu (DropdownMenu / StyledMenuItem): opens at a point, closes on an outside click or
 * Escape.
 */
import { useEffect, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Icon } from "./Icon";
import "./components.css";

export interface MenuItem {
  label: string;
  icon?: string;
  danger?: boolean;
  disabled?: boolean;
  onSelect: () => void;
}

export function Menu({
  x,
  y,
  items,
  onClose,
  header,
}: {
  x: number;
  y: number;
  items: (MenuItem | "divider")[];
  onClose: () => void;
  header?: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const down = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("mousedown", down);
    window.addEventListener("keydown", key);
    return () => {
      window.removeEventListener("mousedown", down);
      window.removeEventListener("keydown", key);
    };
  }, [onClose]);
  // Keep the menu inside the window.
  const left = Math.min(x, window.innerWidth - 220);
  const top = Math.min(y, window.innerHeight - (items.length * 34 + 16));
  return createPortal(
    <div ref={ref} className="nk-menu" style={{ left, top }} role="menu">
      {header}
      {items.map((item, i) =>
        item === "divider" ? (
          <div key={i} className="nk-menu__divider" />
        ) : (
          <button
            key={i}
            type="button"
            role="menuitem"
            disabled={item.disabled}
            className={item.danger ? "nk-menu__item body2 nk-menu__item--danger" : "nk-menu__item body2"}
            onClick={() => {
              onClose();
              item.onSelect();
            }}
          >
            {item.icon && <Icon name={item.icon} size={16} />}
            <span>{item.label}</span>
          </button>
        ),
      )}
    </div>,
    document.body,
  );
}
