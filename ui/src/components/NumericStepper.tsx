/**
 * NumericStepper.kt: a caption label over a bordered box with − / + round buttons at the ends and
 * the value centred. A button is disabled (and dimmed) at its limit.
 */
import { Icon } from "./Icon";
import "./components.css";

export interface NumericStepperProps {
  label: string;
  value: number;
  onChange: (value: number) => void;
  min: number;
  max: number;
  /** 128 by default, as in the Kotlin stepper (context sizes). */
  step?: number;
}

export function NumericStepper({ label, value, onChange, min, max, step = 128 }: NumericStepperProps) {
  const atMin = value <= min;
  const atMax = value >= max;
  return (
    <div className="nk-stepper">
      <span className="caption nk-stepper__label">{label}</span>
      <div className="nk-stepper__box">
        <button
          type="button"
          className="nk-stepper__button"
          aria-label="Decrease"
          disabled={atMin}
          onClick={() => {
            if (!atMin) onChange(value - step);
          }}
        >
          <Icon name="arrow-down" size={12} />
        </button>
        <span className="body1 nk-stepper__value">{value}</span>
        <button
          type="button"
          className="nk-stepper__button"
          aria-label="Increase"
          disabled={atMax}
          onClick={() => {
            if (!atMax) onChange(value + step);
          }}
        >
          <Icon name="arrow-up" size={12} />
        </button>
      </div>
    </div>
  );
}
