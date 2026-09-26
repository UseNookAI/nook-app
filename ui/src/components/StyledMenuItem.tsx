/**
 * StyledMenuItem.kt: a 32 px menu row with a 16 px icon, soft rounded hover, and a warm red for a
 * destructive action. Put it inside a `DropdownMenu` (Popover.tsx). Selecting does not close the
 * menu by itself: the caller closes it in `onClick`, as the Kotlin menus did.
 *
 * `MenuDivider`: the hairline between two groups of rows in a menu.
 */
import type { CSSProperties } from "react";
import { Icon } from "./Icon";
import "./components.css";

export interface StyledMenuItemProps {
  text: string;
  onClick: () => void;
  icon?: string;
  isDestructive?: boolean;
  /** An explicit icon colour (a CSS colour, e.g. "var(--primary-variant)"); wins over the destructive red. */
  tint?: string;
  /** 400 by default; 500 or 700 marks the chosen entry. */
  fontWeight?: CSSProperties["fontWeight"];
  /** Draws the row in the hover colour, for the chosen entry of a list. */
  selected?: boolean;
  disabled?: boolean;
}

export function StyledMenuItem({
  text,
  onClick,
  icon,
  isDestructive = false,
  tint,
  fontWeight,
  selected = false,
  disabled = false,
}: StyledMenuItemProps) {
  const classes = [
    "nk-styled-item",
    isDestructive ? "nk-styled-item--destructive" : "",
    selected ? "nk-styled-item--selected" : "",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <button type="button" role="menuitem" className={classes} disabled={disabled} onClick={onClick} style={{ fontWeight }}>
      {icon && <Icon name={icon} size={16} className="nk-styled-item__icon" color={tint} />}
      <span>{text}</span>
    </button>
  );
}

export function MenuDivider() {
  return <div className="nk-menu__divider" role="separator" />;
}
