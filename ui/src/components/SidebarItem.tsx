/**
 * The sidebar's pieces (hub/sidebar/SidebarItem.kt, SidebarIconButton.kt).
 *
 * `SidebarItem`: one 36 px row, a 16 px icon and a label on a soft rounded hover; `isActive` sits
 * on the selected colour; `accent` draws the icon white in a green disc (the one primary action).
 * Collapsed, only the icon shows, centred, with the label as its tooltip.
 *
 * `SidebarIconButton`: a small square icon button (24 px, 16 px icon) with the rounded hover, for
 * row actions such as a session's menu.
 */
import type { MouseEvent } from "react";
import { Icon } from "./Icon";
import "./components.css";

export interface SidebarItemProps {
  icon: string;
  label: string;
  isCollapsed: boolean;
  isActive?: boolean;
  accent?: boolean;
  onClick?: () => void;
}

export function SidebarItem({ icon, label, isCollapsed, isActive = false, accent = false, onClick }: SidebarItemProps) {
  const classes = [
    "nk-sidebar-item",
    isActive ? "nk-sidebar-item--active" : "",
    accent ? "nk-sidebar-item--accent" : "",
    isCollapsed ? "nk-sidebar-item--collapsed" : "",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <button
      type="button"
      className={classes}
      onClick={onClick}
      title={isCollapsed ? label : undefined}
      aria-label={isCollapsed ? label : undefined}
      aria-current={isActive ? "page" : undefined}
    >
      {accent ? (
        <span className="nk-sidebar-item__disc">
          <Icon name={icon} size={12} />
        </span>
      ) : (
        <Icon name={icon} size={16} className="nk-sidebar-item__icon" />
      )}
      {!isCollapsed && <span className="nk-sidebar-item__label">{label}</span>}
    </button>
  );
}

export interface SidebarIconButtonProps {
  icon: string;
  onClick: (e: MouseEvent<HTMLButtonElement>) => void;
  /** The icon's colour, a CSS colour such as "var(--text-secondary)". */
  tint?: string;
  enabled?: boolean;
  /** The square's side (24 by default). */
  size?: number;
  /** The icon's side (16 by default). */
  iconSize?: number;
  /** A hint and the accessible name. */
  title?: string;
  /** For a row that shows the button only on hover (the session list's bin). */
  className?: string;
}

export function SidebarIconButton({
  icon,
  onClick,
  tint = "var(--text-secondary)",
  enabled = true,
  size = 24,
  iconSize = 16,
  title,
  className,
}: SidebarIconButtonProps) {
  return (
    <button
      type="button"
      className={className ? `nk-sidebar-icon-button ${className}` : "nk-sidebar-icon-button"}
      style={{ width: size, height: size, color: tint }}
      disabled={!enabled}
      title={title}
      aria-label={title}
      onClick={onClick}
    >
      <Icon name={icon} size={iconSize} />
    </button>
  );
}
