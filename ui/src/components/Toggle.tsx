/**
 * An on/off switch (new: the Kotlin screens used an On | Off dropdown instead). Hunter Green when
 * on. `label` names it for screen readers when the row's text sits elsewhere.
 */
import "./components.css";

export interface ToggleProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  label?: string;
}

export function Toggle({ checked, onChange, disabled = false, label }: ToggleProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      title={label}
      disabled={disabled}
      className={checked ? "nk-toggle nk-toggle--on" : "nk-toggle"}
      onClick={() => onChange(!checked)}
    />
  );
}
