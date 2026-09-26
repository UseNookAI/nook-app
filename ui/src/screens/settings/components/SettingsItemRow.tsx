/**
 * settings/components/SettingsItemRow.kt: a row with a title and an optional description on the
 * left and its control (a dropdown, a button, a value) on the right.
 */
import type { ReactNode } from "react";
import "../settings.css";

export function SettingsItemRow({
  title,
  description,
  children,
}: {
  title: ReactNode;
  description?: ReactNode | null;
  /** The control. */
  children?: ReactNode;
}) {
  return (
    <div className="nk-settings-row">
      <div className="nk-settings-row__text">
        <div className="body1 nk-settings-row__title">{title}</div>
        {description != null && description !== "" && <div className="body2 nk-settings-row__description">{description}</div>}
      </div>
      {children != null && <div className="nk-settings-row__control">{children}</div>}
    </div>
  );
}
