/**
 * Settings (popup/settings/SettingsPopup.kt): a rail of four pages and one title per page, over a
 * scrim that closes it. Each page is a flat list of rows; nothing is repeated between the rail,
 * the title and the content.
 *
 * Opened on a tab id: "general", "models", "runtime", "about", or a deep link "<page>/<section>"
 * such as "models/code" (Models opened from Code's model menu: it shows the models Code can use,
 * and picking one closes Settings).
 */
import { useCallback, useEffect, useState } from "react";
import { appEula, appInfo, appNotices, openPath, openUrl, type AppInfo } from "../../api/app";
import { Button } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { useTheme } from "../../shell/theme";
import type { UpdateController } from "../../shell/useUpdate";
import { AboutSettingsView } from "./AboutSettingsView";
import { DocumentDialog } from "./DocumentDialog";
import { GeneralSettingsView } from "./GeneralSettingsView";
import { ModelsTab } from "./tabs/ModelsTab";
import { RuntimeTab } from "./tabs/RuntimeTab";
import "./settings.css";

/** The pages of Settings, in rail order. Ids are what the rest of the app opens by. */
export const SETTINGS_PAGES = [
  { id: "general", label: "General", icon: "general-settings" },
  { id: "models", label: "Models", icon: "models" },
  { id: "runtime", label: "Runtime", icon: "cpu" },
  { id: "about", label: "About", icon: "help" },
] as const;

export type SettingsPageId = (typeof SETTINGS_PAGES)[number]["id"];
type SettingsPage = (typeof SETTINGS_PAGES)[number];

/** SettingsPage.byId: General for anything unknown. */
export function settingsPageById(id: string | null | undefined): SettingsPage {
  return SETTINGS_PAGES.find((p) => p.id === id) ?? SETTINGS_PAGES[0];
}

export interface SettingsPopupProps {
  onDismiss: () => void;
  onNavigateToNuke: () => void;
  /** A page id or a deep link ("models/code"). */
  initialTabId?: string;
  update: UpdateController;
}

type OpenDocument = { title: string; load: () => Promise<string> } | null;

export function SettingsPopup({ onDismiss, onNavigateToNuke, initialTabId = "general", update }: SettingsPopupProps) {
  const slash = initialTabId.indexOf("/");
  const initialPage = settingsPageById(slash >= 0 ? initialTabId.slice(0, slash) : initialTabId);
  const initialSection = slash >= 0 ? initialTabId.slice(slash + 1) : undefined;
  const [page, setPage] = useState<SettingsPage>(initialPage);
  const [headerSlot, setHeaderSlot] = useState<HTMLDivElement | null>(null);
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [openDoc, setOpenDoc] = useState<OpenDocument>(null);
  const { mode, setMode } = useTheme();
  const status = update.status;

  useEffect(() => {
    appInfo()
      .then(setInfo)
      .catch(() => setInfo(null));
  }, []);

  const closeDocument = useCallback(() => setOpenDoc(null), []);
  const sectionFor = (p: SettingsPage) => (p.id === initialPage.id ? initialSection : undefined);

  return (
    <div
      className="nk-settings-scrim"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onDismiss();
      }}
    >
      <div className="nk-settings" role="dialog" aria-label="Settings">
        <nav className="nk-settings__rail">
          <div className="overline nk-settings__rail-title">Settings</div>
          {SETTINGS_PAGES.filter((p) => p.id !== "about").map((p) => (
            <RailItem key={p.id} page={p} active={page.id === p.id} onClick={() => setPage(p)} />
          ))}
          <div className="nk-settings__rail-spacer" />
          {status?.isUpdateAvailable && (
            <Button
              variant="soft"
              className="nk-settings__update"
              text={status.latestVersionInfo?.version ? `Update to ${status.latestVersionInfo.version}` : "Update available"}
              onClick={() => update.setPopupVisible(true)}
            />
          )}
          <RailItem page={SETTINGS_PAGES[3]} active={page.id === "about"} onClick={() => setPage(SETTINGS_PAGES[3])} />
        </nav>

        <div className="nk-settings__page">
          <div className="nk-settings__header">
            <span className="h5 nk-settings__title">{page.label}</span>
            <div ref={setHeaderSlot} className="nk-settings__slot" />
            <button type="button" className="nk-settings__close" aria-label="Close" onClick={onDismiss}>
              <Icon name="close" size={12} />
            </button>
          </div>
          <div key={page.id} className="nk-settings__body">
            {page.id === "general" && (
              <GeneralSettingsView
                currentTheme={mode}
                onThemeChange={setMode}
                onNavigateToNuke={onNavigateToNuke}
                updateChannel={status?.channel ?? "stable"}
                onUpdateChannelChange={update.chooseUpdateChannel}
                updateCheckError={status?.lastCheckError ?? null}
                updateWaitingNote={status?.waitingNote ?? null}
                onCheckForUpdates={update.checkForUpdates}
              />
            )}
            {page.id === "models" && <ModelsTab initialSection={sectionFor(page)} onPicked={onDismiss} headerSlot={headerSlot} />}
            {page.id === "runtime" && <RuntimeTab initialSection={sectionFor(page)} />}
            {page.id === "about" && (
              <AboutSettingsView
                version={info?.label ?? status?.buildLabel ?? ""}
                home={info?.home ?? null}
                onOpenHome={() => info && openPath(info.home).catch(() => {})}
                onOpenLicence={() => setOpenDoc({ title: "Licence", load: appEula })}
                onOpenNotices={() => setOpenDoc({ title: "Third-party notices", load: appNotices })}
                onOpenUrl={(url) => openUrl(url).catch(() => {})}
              />
            )}
          </div>
        </div>
      </div>
      {openDoc && <DocumentDialog title={openDoc.title} load={openDoc.load} onDismiss={closeDocument} />}
    </div>
  );
}

function RailItem({ page, active, onClick }: { page: SettingsPage; active: boolean; onClick: () => void }) {
  return (
    <button
      type="button"
      className={active ? "nk-rail-item nk-rail-item--active" : "nk-rail-item"}
      aria-current={active ? "page" : undefined}
      onClick={onClick}
    >
      <Icon name={page.icon} size={16} className="nk-rail-item__icon" />
      <span>{page.label}</span>
    </button>
  );
}
