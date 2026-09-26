/**
 * The theme (NookAgentApplication.App + SettingsViewModel.appThemeMode): Light, Dark or System
 * from the APP_THEME setting; System follows Windows. The effective theme is set as
 * `document.documentElement.dataset.theme` ("light" | "dark"), which theme.css keys on.
 *
 *   const { mode, dark, setMode } = useTheme();
 */
import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useState, type ReactNode } from "react";
import { Setting, settingsSet } from "../api/app";

export type AppThemeMode = "Light" | "Dark" | "System";
export const APP_THEME_MODES: readonly AppThemeMode[] = ["Light", "Dark", "System"];

/** AppThemeMode.valueOf, falling back to System for anything it does not know. */
export function parseThemeMode(value: string | null | undefined): AppThemeMode {
  return APP_THEME_MODES.find((m) => m === value) ?? "System";
}

interface ThemeApi {
  mode: AppThemeMode;
  /** The theme in effect: Dark, or System while Windows is dark. */
  dark: boolean;
  /** Applies and saves a mode (NookAgentService.updateAppTheme). */
  setMode: (mode: AppThemeMode) => void;
}

const Ctx = createContext<ThemeApi>({ mode: "Light", dark: false, setMode: () => {} });

const systemQuery = () => (typeof window !== "undefined" ? window.matchMedia("(prefers-color-scheme: dark)") : null);

function useSystemDark(): boolean {
  const [dark, setDark] = useState(() => systemQuery()?.matches ?? false);
  useEffect(() => {
    const q = systemQuery();
    if (!q) return;
    const change = (e: MediaQueryListEvent) => setDark(e.matches);
    q.addEventListener("change", change);
    return () => q.removeEventListener("change", change);
  }, []);
  return dark;
}

export function ThemeProvider({ initialMode, children }: { initialMode: AppThemeMode; children: ReactNode }) {
  const [mode, setModeState] = useState<AppThemeMode>(initialMode);
  const systemDark = useSystemDark();
  const dark = mode === "Dark" || (mode === "System" && systemDark);

  useLayoutEffect(() => {
    document.documentElement.dataset.theme = dark ? "dark" : "light";
  }, [dark]);

  const setMode = useCallback((next: AppThemeMode) => {
    setModeState(next);
    settingsSet(Setting.APP_THEME, next).catch((e) => console.warn("Could not save the theme", e));
  }, []);

  const api = useMemo(() => ({ mode, dark, setMode }), [mode, dark, setMode]);
  return <Ctx.Provider value={api}>{children}</Ctx.Provider>;
}

export const useTheme = () => useContext(Ctx);
