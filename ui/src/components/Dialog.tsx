/**
 * A modal over the whole window (Compose Dialog inside AppPortal): a scrim and a surface card.
 * Escape and a scrim click call onDismiss when given.
 */
import { useEffect, type ReactNode } from "react";
import { createPortal } from "react-dom";
import "./components.css";

export function Dialog({
  title,
  children,
  actions,
  onDismiss,
  width = 440,
}: {
  title?: ReactNode;
  children?: ReactNode;
  actions?: ReactNode;
  onDismiss?: () => void;
  width?: number;
}) {
  useEffect(() => {
    if (!onDismiss) return;
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") onDismiss();
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [onDismiss]);
  return createPortal(
    <div
      className="nk-scrim"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onDismiss?.();
      }}
    >
      <div className="nk-dialog" style={{ width }} role="dialog">
        {title && <div className="h6 nk-dialog__title">{title}</div>}
        {children && <div className="body2 text-secondary">{children}</div>}
        {actions && <div className="nk-dialog__actions">{actions}</div>}
      </div>
    </div>,
    document.body,
  );
}
