/** SettingsPopup.kt's SettingsSubNav: a segmented control for pages that have a few views (Models). */
import "../settings.css";

export function SettingsSubNav({
  options,
  selectedIndex,
  onOptionSelected,
}: {
  options: readonly string[];
  selectedIndex: number;
  onOptionSelected: (index: number) => void;
}) {
  return (
    <div className="nk-subnav" role="tablist">
      {options.map((label, index) => (
        <button
          key={label}
          type="button"
          role="tab"
          aria-selected={index === selectedIndex}
          className={index === selectedIndex ? "nk-subnav__item nk-subnav__item--selected" : "nk-subnav__item"}
          onClick={() => onOptionSelected(index)}
        >
          {label}
        </button>
      ))}
    </div>
  );
}
