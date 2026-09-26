/**
 * Small pieces shared by the pages (home/ActivityComponents.kt).
 *
 * `Chip`: a quiet label (local model names, tool counts); `accent` is pale green.
 * `LiveDot`: an 8 px green dot that breathes while something runs; lavender while the model thinks.
 * `QuietAction`: a quiet text button with an optional 14 px icon.
 * `TextLink` (code/CodeControls.kt): an underlined link, for the quieter choice under a button.
 * Modifier.pageWidth() is the CSS class `nk-page-width` (at most --max-content-width wide).
 */
import type { MouseEvent } from "react";
import { Icon } from "./Icon";
import "./components.css";

export function Chip({ text, accent = false, icon }: { text: string; accent?: boolean; icon?: string }) {
  return (
    <span className={accent ? "nk-chip nk-chip--accent" : "nk-chip"}>
      {icon && <Icon name={icon} size={12} />}
      {text}
    </span>
  );
}

/** `thinking` draws it in Vintage Lavender; `color` (a CSS colour) overrides both. */
export function LiveDot({ color, thinking = false }: { color?: string; thinking?: boolean }) {
  return (
    <span
      className={thinking ? "nk-live-dot nk-live-dot--thinking" : "nk-live-dot"}
      style={color ? { background: color } : undefined}
      aria-hidden
    />
  );
}

export function QuietAction({
  text,
  onClick,
  icon,
  disabled = false,
  title,
}: {
  text: string;
  onClick: (e: MouseEvent<HTMLButtonElement>) => void;
  icon?: string;
  disabled?: boolean;
  title?: string;
}) {
  return (
    <button type="button" className="nk-quiet-action" onClick={onClick} disabled={disabled} title={title}>
      {icon && <Icon name={icon} size={14} className="nk-quiet-action__icon" />}
      <span>{text}</span>
    </button>
  );
}

/** "Not now", "Leave it out": an underlined link in the secondary colour. */
export function TextLink({ text, onClick }: { text: string; onClick: () => void }) {
  return (
    <button type="button" className="nk-text-link" onClick={onClick}>
      {text}
    </button>
  );
}
