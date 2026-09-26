/** settings/components/SettingsDropdown.kt: a settings row whose control is a pick-one pill. */
import type { ReactNode } from "react";
import { Dropdown, type DropdownOption } from "../../../components/Dropdown";
import { SettingsItemRow } from "./SettingsItemRow";

export function SettingsDropdown<T extends string = string>({
  title,
  description,
  options,
  selectedValue,
  onOptionSelect,
}: {
  title: string;
  description?: ReactNode | null;
  options: readonly (T | DropdownOption<T>)[];
  selectedValue: T;
  onOptionSelect: (value: T) => void;
}) {
  return (
    <SettingsItemRow title={title} description={description}>
      <Dropdown options={options} value={selectedValue} onChange={onOptionSelect} ariaLabel={title} />
    </SettingsItemRow>
  );
}
