/** Version, where the data lives, the licence and where to find us (AboutSettingsView.kt). */
import { SettingsAction, SettingsGroup, SettingsItemRow, SettingsRowDivider, SettingsView } from "./components";

const X_URL = "https://x.com/usenook";

export interface AboutSettingsViewProps {
  /** The build label, "0.5.0 (abc1234, 2026-09-25)". */
  version: string;
  /** The Nook home folder, or null when unknown. */
  home: string | null;
  onOpenHome: () => void;
  onOpenLicence: () => void;
  onOpenUrl: (url: string) => void;
  onOpenNotices: () => void;
}

export function AboutSettingsView({ version, home, onOpenHome, onOpenLicence, onOpenUrl, onOpenNotices }: AboutSettingsViewProps) {
  return (
    <SettingsView>
      <SettingsGroup>
        <SettingsItemRow title="Version">
          <span className="body2 nk-settings-version selectable">{version}</span>
        </SettingsItemRow>
        {home != null && (
          <>
            <SettingsRowDivider />
            <SettingsItemRow title="Data folder" description={<span className="selectable">{home}</span>}>
              <SettingsAction text="Open" onClick={onOpenHome} />
            </SettingsItemRow>
          </>
        )}
        <SettingsRowDivider />
        <SettingsItemRow title="Licence">
          <SettingsAction text="View" onClick={onOpenLicence} />
        </SettingsItemRow>
        <SettingsRowDivider />
        <SettingsItemRow
          title="Third-party notices"
          description="The open-source engines, libraries, fonts and Java runtime Nook is built on."
        >
          <SettingsAction text="View" onClick={onOpenNotices} />
        </SettingsItemRow>
        <SettingsRowDivider />
        <SettingsItemRow title="Follow along" description="@usenook on X">
          <SettingsAction text="Open" onClick={() => onOpenUrl(X_URL)} />
        </SettingsItemRow>
      </SettingsGroup>
    </SettingsView>
  );
}
