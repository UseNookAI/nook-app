/**
 * CustomButton.kt: a pill button (36 px: 8 px above and below the 20 px label, 16 px at the sides).
 * Variants replace the Kotlin colour arguments: "primary" (Carbon Black fill), "accent" (Hunter
 * Green fill), "secondary" (outlined), "ghost" (transparent, secondary text; the dialogs' Cancel /
 * Later), "danger" (the error red: the erase screen's Nuke), "soft" (pale green: Settings' "Update
 * to ..."). Hover lightens the fill as CustomButton's `lighten(0.15f)` did.
 */
import type { ButtonHTMLAttributes, ReactNode } from "react";
import { Icon } from "./Icon";
import { Tooltip } from "./Tooltip";
import "./components.css";

export type ButtonVariant = "primary" | "accent" | "secondary" | "ghost" | "danger" | "soft";

export interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> {
  text?: string;
  variant?: ButtonVariant;
  icon?: string;
  iconPosition?: "start" | "end";
  compact?: boolean;
  children?: ReactNode;
}

export function Button({
  text,
  variant = "primary",
  icon,
  iconPosition = "end",
  compact = false,
  className,
  children,
  ...rest
}: ButtonProps) {
  const label = text ?? children;
  const iconEl = icon ? <Icon name={icon} size={label ? 16 : 20} /> : null;
  const classes = [
    "nk-button",
    `nk-button--${variant}`,
    compact ? "nk-button--compact" : "",
    label ? "" : "nk-button--icon",
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <button type="button" className={classes} {...rest}>
      {iconPosition === "start" && iconEl}
      {label && <span>{label}</span>}
      {iconPosition === "end" && iconEl}
    </button>
  );
}

/** A square icon button (sidebar and title strip buttons, toolbars). */
export function IconButton({
  icon,
  size = 32,
  iconSize = 16,
  title,
  active = false,
  className,
  style,
  ...rest
}: Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> & {
  icon: string;
  size?: number;
  iconSize?: number;
  active?: boolean;
}) {
  const classes = ["nk-icon-button", active ? "nk-icon-button--active" : "", className ?? ""].filter(Boolean).join(" ");
  return (
    <button type="button" title={title} aria-label={title} className={classes} style={{ width: size, height: size, ...style }} {...rest}>
      <Icon name={icon} size={iconSize} />
    </button>
  );
}

/**
 * ReplyParts.kt's ToolbarIcon: a 28 px icon button (15 px icon) with its name in a tooltip under
 * it, for the small actions of a reply, a code block and the Code page's panes. `tint` fixes the
 * icon's colour, hovered or not.
 */
export function ToolbarIcon({ icon, hint, onClick, tint }: { icon: string; hint: string; onClick: () => void; tint?: string }) {
  return (
    <Tooltip text={hint} placement="bottom" delay={400}>
      <IconButton icon={icon} size={28} iconSize={15} aria-label={hint} onClick={onClick} style={tint ? { color: tint } : undefined} />
    </Tooltip>
  );
}
