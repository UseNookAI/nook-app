/**
 * The window (NookAgentApplication.App): the settings are read once, then the theme and the
 * snackbar host wrap the shell (shell/AppShell.tsx), which routes between the welcome flow, the hub
 * and the erase screen and owns the title strip, Settings and the update and quit dialogs.
 */
import { useEffect, useState } from "react";
import { Setting, settingsAll } from "./api/app";
import { SnackbarProvider } from "./components/Snackbar";
import { AppShell } from "./shell/AppShell";
import { parseThemeMode, ThemeProvider } from "./shell/theme";

export function App() {
  const [settings, setSettings] = useState<Record<string, string> | null>(null);
  useEffect(() => {
    settingsAll()
      .then(setSettings)
      .catch((e) => {
        console.warn("Settings unavailable", e);
        setSettings({});
      });
  }, []);

  // A blank canvas for the moment it takes to read the settings.
  if (!settings) return <div className="nk-app" />;
  return (
    <ThemeProvider initialMode={parseThemeMode(settings[Setting.APP_THEME])}>
      <SnackbarProvider>
        <AppShell settings={settings} />
      </SnackbarProvider>
    </ThemeProvider>
  );
}
