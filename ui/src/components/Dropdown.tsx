/**
 * A pick-one control: SettingsDropdown.kt's pill trigger (the value, a chevron that turns while
 * open) and its menu, where the chosen option is bold on the hover colour.
 *
 *   <Dropdown options={["Light", "Dark", "System"]} value={mode} onChange={setMode} />
 *   <Dropdown options={[{ value: "on", label: "On" }, { value: "off", label: "Off" }]} ... />
 */
import { useRef, useState } from "react";
import { Icon } from "./Icon";
import { DropdownMenu } from "./Popover";
import "./components.css";

export interface DropdownOption<T extends string = string> {
  value: T;
  label?: string;
}

export interface DropdownProps<T extends string = string> {
  options: readonly (T | DropdownOption<T>)[];
  value: T;
  onChange: (value: T) => void;
  /** The trigger's width range (SettingsDropdown: 100 to 120 px). */
  minWidth?: number;
  maxWidth?: number;
  disabled?: boolean;
  /** For screen readers when no visible label sits beside it. */
  ariaLabel?: string;
}

const normalise = <T extends string>(o: T | DropdownOption<T>): DropdownOption<T> =>
  typeof o === "string" ? { value: o, label: o } : { value: o.value, label: o.label ?? o.value };

export function Dropdown<T extends string = string>({
  options,
  value,
  onChange,
  minWidth = 100,
  maxWidth = 120,
  disabled = false,
  ariaLabel,
}: DropdownProps<T>) {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const all = options.map(normalise);
  const current = all.find((o) => o.value === value);
  return (
    <>
      <button
        ref={trigger}
        type="button"
        className="nk-select"
        style={{ minWidth, maxWidth }}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={ariaLabel}
        onClick={() => setOpen(!open)}
      >
        <span className="nk-select__value">{current?.label ?? value}</span>
        <Icon name="arrow-down" size={14} className={open ? "nk-select__chevron nk-select__chevron--open" : "nk-select__chevron"} />
      </button>
      <DropdownMenu anchor={trigger} open={open} onClose={() => setOpen(false)}>
        <div role="listbox">
          {all.map((o) => (
            <button
              key={o.value}
              type="button"
              role="option"
              aria-selected={o.value === value}
              className={o.value === value ? "nk-select-item nk-select-item--selected" : "nk-select-item"}
              onClick={() => {
                onChange(o.value);
                setOpen(false);
              }}
            >
              {o.label}
            </button>
          ))}
        </div>
      </DropdownMenu>
    </>
  );
}
