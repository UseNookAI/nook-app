/** Appearance, updates and erasing Nook's data, on one page (GeneralSettingsView.kt). */
import type { UpdateChannel } from "../../api/update";
import { APP_THEME_MODES, type AppThemeMode } from "../../shell/theme";
import {
  SettingsAction,
  SettingsDropdown,
  SettingsGroup,
  SettingsItemRow,
  SettingsRowDivider,
  SettingsView,
} from "./components";

export interface GeneralSettingsViewProps {
  currentTheme: AppThemeMode;
  onThemeChange: (mode: AppThemeMode) => void;
  onNavigateToNuke: () => void;
  updateChannel?: UpdateChannel;
  onUpdateChannelChange?: (channel: UpdateChannel) => void;
  updateCheckError?: string | null;
  updateWaitingNote?: string | null;
  onCheckForUpdates?: () => void;
}

const CHANNELS: readonly UpdateChannel[] = ["stable", "dev"];

export function GeneralSettingsView({
  currentTheme,
  onThemeChange,
  onNavigateToNuke,
  updateChannel = "stable",
  onUpdateChannelChange = () => {},
  updateCheckError = null,
  updateWaitingNote = null,
  onCheckForUpdates = () => {},
}: GeneralSettingsViewProps) {
  // docs/plan/distribution.md: stable asks before it installs a new build, dev installs it as it comes
  const channelText =
    updateChannel === "dev"
      ? "Dev: each new build is installed without asking, once nothing is running."
      : "Stable: Nook asks before it installs a new build.";
  const checkText =
    updateCheckError ??
    updateWaitingNote ??
    (updateChannel === "dev"
      ? "Checked every minute; every manifest is verified against Nook's release keys first."
      : "Checked every fifteen minutes; every manifest is verified against Nook's release keys first.");

  return (
    <SettingsView>
      <SettingsGroup>
        <SettingsDropdown title="Theme" options={APP_THEME_MODES} selectedValue={currentTheme} onOptionSelect={onThemeChange} />
      </SettingsGroup>

      <SettingsGroup title="Updates">
        <SettingsDropdown
          title="Channel"
          description={channelText}
          options={CHANNELS}
          selectedValue={updateChannel}
          onOptionSelect={onUpdateChannelChange}
        />
        <SettingsRowDivider />
        <SettingsItemRow title="Check now" description={checkText}>
          <SettingsAction text="Check" onClick={onCheckForUpdates} />
        </SettingsItemRow>
      </SettingsGroup>

      <SettingsGroup title="Data">
        <SettingsItemRow title="Erase everything" description="Chats, downloaded models, reports and keys. Nook starts over.">
          <SettingsAction text="Erase…" danger onClick={onNavigateToNuke} />
        </SettingsItemRow>
      </SettingsGroup>
    </SettingsView>
  );
}
