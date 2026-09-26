/**
 * Text inputs.
 *
 * `TextField`: the bordered single-line field of the Kotlin dialogs (IdeDialogs' name field, the
 * erase screen's "Type 'nuke'"): a strong hairline, the page colour inside, body1 text. Enter and
 * Escape can be handled directly. `tone="danger"` turns the focused border and caret red.
 *
 * `SearchField`: the pill with a magnifier from the Settings header (ModelsSearchField).
 */
import type { InputHTMLAttributes, KeyboardEvent, Ref } from "react";
import { Icon } from "./Icon";
import "./components.css";

type InputProps = Omit<InputHTMLAttributes<HTMLInputElement>, "onChange" | "value" | "size">;

export interface TextFieldProps extends InputProps {
  value: string;
  onChange: (value: string) => void;
  onEnter?: () => void;
  onEscape?: () => void;
  tone?: "default" | "danger";
  /** Draws the border in the error colour. */
  invalid?: boolean;
  ref?: Ref<HTMLInputElement>;
}

function keys(onEnter?: () => void, onEscape?: () => void, own?: (e: KeyboardEvent<HTMLInputElement>) => void) {
  return (e: KeyboardEvent<HTMLInputElement>) => {
    own?.(e);
    if (e.defaultPrevented) return;
    if (e.key === "Enter" && onEnter) {
      e.preventDefault();
      onEnter();
    } else if (e.key === "Escape" && onEscape) {
      e.preventDefault();
      e.stopPropagation();
      onEscape();
    }
  };
}

export function TextField({
  value,
  onChange,
  onEnter,
  onEscape,
  tone = "default",
  invalid = false,
  className,
  onKeyDown,
  ref,
  ...rest
}: TextFieldProps) {
  const classes = [
    "nk-textfield",
    tone === "danger" ? "nk-textfield--danger" : "",
    invalid ? "nk-textfield--invalid" : "",
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <input
      ref={ref}
      type="text"
      spellCheck={false}
      autoComplete="off"
      {...rest}
      className={classes}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={keys(onEnter, onEscape, onKeyDown)}
    />
  );
}

export interface SearchFieldProps extends InputProps {
  value: string;
  onChange: (value: string) => void;
  onEnter?: () => void;
  onEscape?: () => void;
  /** The pill's width; it fills its parent when omitted. */
  width?: number | string;
  ref?: Ref<HTMLInputElement>;
}

export function SearchField({ value, onChange, onEnter, onEscape, width, className, onKeyDown, ref, ...rest }: SearchFieldProps) {
  return (
    <div className={["nk-searchfield", className ?? ""].filter(Boolean).join(" ")} style={{ width }}>
      <Icon name="search" size={16} />
      <input
        ref={ref}
        type="text"
        spellCheck={false}
        autoComplete="off"
        {...rest}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={keys(onEnter, onEscape, onKeyDown)}
      />
    </div>
  );
}
