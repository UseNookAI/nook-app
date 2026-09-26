/**
 * settings/components/SettingsGroup.kt.
 *
 * `SettingsGroup`: a titled card of settings rows. Rows are separated by `SettingsRowDivider`
 * hairlines; the title is optional and sits above the card in the tertiary colour.
 * `SettingsAction`: a quiet, bordered action for the right side of a row; `danger` turns it warm on
 * hover, `primary` fills it.
 * `SettingsView`: the scrolling column a settings page lays its groups in (General, About).
 */
import type { ReactNode } from "react";
import "../settings.css";

export function SettingsGroup({ title, children, className }: { title?: string; children: ReactNode; className?: string }) {
  return (
    <section className={["nk-settings-group", className ?? ""].filter(Boolean).join(" ")}>
      {title && <div className="overline nk-settings-group__title">{title}</div>}
      <div className="nk-settings-group__card">{children}</div>
    </section>
  );
}

/** The hairline between two rows of a SettingsGroup. */
export function SettingsRowDivider() {
  return <div className="nk-settings-divider" role="separator" />;
}

export function SettingsAction({
  text,
  onClick,
  primary = false,
  danger = false,
  enabled = true,
}: {
  text: string;
  onClick: () => void;
  primary?: boolean;
  danger?: boolean;
  enabled?: boolean;
}) {
  const classes = [
    "nk-settings-action",
    primary ? "nk-settings-action--primary" : "",
    danger ? "nk-settings-action--danger" : "",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <button type="button" className={classes} disabled={!enabled} onClick={onClick}>
      {text}
    </button>
  );
}

/** A settings page's scrolling column: 8 px above the first group, 16 px below the last. */
export function SettingsView({ children }: { children: ReactNode }) {
  return <div className="nk-settings-view">{children}</div>;
}
