/**
 * The composer's controls (ComposerControls.kt), shared by the Chat, Code and Video pages.
 *
 * `ToolbarPill`: a quiet pill for the composer toolbar: label, optional leading icon and a chevron
 * (or a lock). Pass `ref` to anchor a `NookDropdownMenu` to it.
 * `NookDropdownMenu`: the menu surface (components/Popover's DropdownMenu).
 * `PromptActionIconButton`: send, stop or a spinner, as a 32 px disc.
 */
import type { Ref } from "react";
import { Icon } from "../../components/Icon";
import { Spinner } from "../../components/Spinner";
import "./hub.css";

export { DropdownMenu as NookDropdownMenu } from "../../components/Popover";

export interface ToolbarPillProps {
  text: string;
  onClick: () => void;
  icon?: string;
  enabled?: boolean;
  /** The menu it opens is open: the chevron turns. */
  expanded?: boolean;
  /** A lock instead of the chevron. */
  locked?: boolean;
  /** False for a pill that acts at once instead of opening a menu. */
  chevron?: boolean;
  /** Pale green with the accent text. */
  emphasised?: boolean;
  maxWidth?: number;
  title?: string;
  ref?: Ref<HTMLButtonElement>;
}

export function ToolbarPill({
  text,
  onClick,
  icon,
  enabled = true,
  expanded = false,
  locked = false,
  chevron = true,
  emphasised = false,
  maxWidth = 220,
  title,
  ref,
}: ToolbarPillProps) {
  const classes = ["nk-toolbar-pill", emphasised ? "nk-toolbar-pill--emphasised" : ""].filter(Boolean).join(" ");
  return (
    <button ref={ref} type="button" className={classes} style={{ maxWidth }} disabled={!enabled} onClick={onClick} title={title}>
      {icon && <Icon name={icon} size={14} />}
      <span className="nk-toolbar-pill__text">{text}</span>
      {(locked || chevron) && (
        <Icon
          name={locked ? "lock" : "arrow-down"}
          size={locked ? 11 : 10}
          className={!locked && expanded ? "nk-toolbar-pill__chevron nk-toolbar-pill__chevron--open" : "nk-toolbar-pill__chevron"}
        />
      )}
    </button>
  );
}

export interface PromptActionIconButtonProps {
  isGenerating: boolean;
  isLoading?: boolean;
  isEnabled: boolean;
  onSend: () => void;
  onCancel: () => void;
}

export function PromptActionIconButton({ isGenerating, isLoading = false, isEnabled, onSend, onCancel }: PromptActionIconButtonProps) {
  if (isLoading) {
    return (
      <span className="nk-prompt-action nk-prompt-action--loading">
        <Spinner size={14} stroke={2} color="var(--text-secondary)" trackColor="transparent" />
      </span>
    );
  }
  if (isGenerating) {
    return (
      <button type="button" className="nk-prompt-action" title="Stop" aria-label="Stop" onClick={onCancel}>
        <span className="nk-prompt-action__stop" />
      </button>
    );
  }
  return (
    <button type="button" className="nk-prompt-action" title="Send" aria-label="Send" disabled={!isEnabled} onClick={onSend}>
      <Icon name="arrow-up-line" size={16} />
    </button>
  );
}
