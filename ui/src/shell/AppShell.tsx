/**
 * The window (NookAgentApplication.App): the welcome screen until it has been completed once for
 * the current flow version, then the hub; the erase screen; the title strip; Settings, and the
 * update and quit dialogs. The window is sized per screen as the Kotlin App did.
 */
import { lazy, Suspense, useCallback, useEffect, useState } from "react";
import { quit, Setting, settingsSet } from "../api/app";
import { inTauri } from "../api/ipc";
import { updateCancel } from "../api/update";
import { HubNav } from "../screens/code/HubNav";
import { HubScreen } from "../screens/hub/HubScreen";
import { NukeScreen } from "../screens/nuke/NukeScreen";
import { SettingsPopup } from "../screens/settings/SettingsPopup";
import { WELCOME_FLOW_VERSION, WelcomeScreen } from "../screens/welcome/WelcomeScreen";
import { busyKeys, BusyKey, useBusyKeys, useBusyReporter } from "./busy";
import { QuitDialog } from "./QuitDialog";
import { useTheme } from "./theme";
import { SetupHeader, TopBar } from "./TopBar";
import { UpdateDialog } from "./UpdateDialog";
import { useUpdate } from "./useUpdate";
import { applyWindowMode, useCloseRequest } from "./window";
import "./shell.css";

export type Screen = "welcome" | "hub" | "nuke";

/** The initial route: the welcome screen until it has been completed once for the current flow version. */
export function startScreen(settings: Record<string, string>): Screen {
  const raw = settings[Setting.SETUP_VERSION]?.trim() ?? "";
  const installedSetupVersion = /^[+-]?\d+$/.test(raw) ? parseInt(raw, 10) : 0;
  const setupCompleted = settings[Setting.IS_SETUP_COMPLETED]?.trim().toLowerCase() === "true";
  return !setupCompleted || installedSetupVersion < WELCOME_FLOW_VERSION ? "welcome" : "hub";
}

/**
 * Browser preview only: `?settings=<tab id>` opens Settings on that tab at start; `?gallery` shows
 * every shared component in place of the hub.
 */
const preview = !inTauri && import.meta.env.DEV ? new URLSearchParams(window.location.search) : null;
const previewSettingsTab = preview?.get("settings") ?? null;
const DevGallery = preview?.has("gallery") ? lazy(() => import("./DevGallery").then((m) => ({ default: m.DevGallery }))) : null;

export function AppShell({ settings }: { settings: Record<string, string> }) {
  const [screen, setScreen] = useState<Screen>(() => startScreen(settings));
  const [isSidebarCollapsed, setIsSidebarCollapsed] = useState(false);
  // Settings visibility + which tab to land on when opened
  const [showSettings, setShowSettings] = useState(previewSettingsTab !== null && screen === "hub");
  const [settingsInitialTab, setSettingsInitialTab] = useState(previewSettingsTab || "general");
  const [showExitDialog, setShowExitDialog] = useState(false);
  const { dark, setMode } = useTheme();
  const update = useUpdate();
  const busy = useBusyKeys();
  const status = update.status;

  useBusyReporter(BusyKey.UPDATE, status?.isDownloading ?? false);

  useEffect(() => {
    if (screen === "hub") settingsSet(Setting.LAST_ACTIVE_SCREEN, screen).catch(() => {});
    if (screen === "welcome") applyWindowMode(false, 1200, 800);
    else applyWindowMode(true, 1280, 720);
  }, [screen]);

  const requestWindowClose = useCallback(() => {
    if (busyKeys().length > 0) setShowExitDialog(true);
    else quit().catch(() => {});
  }, []);
  useCloseRequest(requestWindowClose);

  const confirmExit = async () => {
    if (status?.isDownloading) await updateCancel().catch(() => {});
    await quit().catch(() => {});
    setShowExitDialog(false);
  };

  const toggleSidebar = useCallback(() => setIsSidebarCollapsed((c) => !c), []);
  const openSettings = useCallback((tabId: string) => {
    setSettingsInitialTab(tabId);
    setShowSettings(true);
  }, []);

  const isSetupPhase = screen === "welcome" || screen === "nuke";

  return (
    <div className={isSetupPhase ? "nk-app nk-app--setup" : "nk-app"}>
      {!isSetupPhase && (
        <TopBar
          isSidebarVisible
          isSidebarCollapsed={isSidebarCollapsed}
          onToggleSidebar={toggleSidebar}
          showUpdateBadge={!!status?.isUpdateAvailable && !status.isDownloading}
          onUpdateClick={() => update.setPopupVisible(true)}
          onClose={requestWindowClose}
          isDarkTheme={dark}
          // A click picks the other theme outright; Settings still offers System.
          onToggleTheme={() => setMode(dark ? "Light" : "Dark")}
          nav={screen === "hub" && !DevGallery ? <HubNav /> : undefined}
        />
      )}

      <div className="nk-app__content">
        {screen === "welcome" && <WelcomeScreen onProceed={() => setScreen("hub")} />}
        {screen === "hub" && DevGallery && (
          <Suspense>
            <DevGallery />
          </Suspense>
        )}
        {screen === "hub" && !DevGallery && (
          <HubScreen isSidebarCollapsed={isSidebarCollapsed} onToggleSidebar={toggleSidebar} onSettings={openSettings} />
        )}
        {screen === "nuke" && <NukeScreen onProceed={() => setScreen("welcome")} onReturn={() => setScreen("hub")} />}
      </div>

      {isSetupPhase && <SetupHeader onClose={requestWindowClose} onScrim={screen === "nuke"} />}

      {showSettings && (
        <SettingsPopup
          initialTabId={settingsInitialTab}
          update={update}
          onDismiss={() => setShowSettings(false)}
          onNavigateToNuke={() => {
            setShowSettings(false);
            setScreen("nuke");
          }}
        />
      )}

      {update.popupVisible && status && (
        <UpdateDialog
          status={status}
          onLater={update.snoozeUpdate}
          onUpdateNow={() => {
            update.startUpdate();
            setShowSettings(false);
          }}
          onCancel={() => {
            update.cancelUpdate();
            update.setPopupVisible(false);
          }}
        />
      )}

      {showExitDialog && <QuitDialog busy={busy} onDismiss={() => setShowExitDialog(false)} onConfirm={confirmExit} />}
    </div>
  );
}
