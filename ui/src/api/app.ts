/** App-level commands (src-tauri/src/commands/app.rs). */
import { call, inTauri } from "./ipc";

export interface BuildInfo {
  version: string;
  commit: string;
  time: string | null;
}

export interface AppInfo {
  build: BuildInfo;
  /** "0.5.0 (abc1234, 2026-09-25)" */
  label: string;
  home: string;
}

/** Setting names (nook_core::settings). Values are strings. */
export const Setting = {
  IS_EULA_ACCEPTED: "IS_EULA_ACCEPTED",
  IS_SETUP_COMPLETED: "IS_SETUP_COMPLETED",
  SETUP_VERSION: "SETUP_VERSION",
  USAGE_PREFERENCES: "USAGE_PREFERENCES",
  APP_THEME: "APP_THEME",
  IS_ADVANCED_MODE: "IS_ADVANCED_MODE",
  LAST_ACTIVE_SCREEN: "LAST_ACTIVE_SCREEN",
  UPDATE_CHANNEL: "UPDATE_CHANNEL",
} as const;

export const appInfo = () => call<AppInfo>("app_info");
export const settingsAll = () => call<Record<string, string>>("settings_all");
export const settingsSet = (name: string, value: string) => call<void>("settings_set", { name, value });
/** Settings back to defaults and the log emptied (NookAgentService.nukeInstance). Quit afterwards. */
export const eraseEverything = () => call<void>("app_erase_everything");
export const quit = () => call<void>("app_quit");
/** What quitting now would cut short or lose, in words, from the core (an edited PDF, a running Nooklet). */
export const quitCheck = () => call<string[]>("app_quit_check");

/** The software licence (resources/eula.html), a complete HTML document. */
export const appEula = () => call<string>("app_eula");
/** The third-party notices (resources/third-party-notices.html), a complete HTML document. */
export const appNotices = () => call<string>("app_notices");

/** Opens a web address in the default browser (tauri-plugin-opener). */
export async function openUrl(url: string): Promise<void> {
  if (inTauri) {
    const opener = await import("@tauri-apps/plugin-opener");
    await opener.openUrl(url);
  } else {
    window.open(url, "_blank", "noopener");
  }
}

/** Opens a folder or file with its default handler: Explorer for a folder (tauri-plugin-opener). */
export async function openPath(path: string): Promise<void> {
  if (inTauri) {
    const opener = await import("@tauri-apps/plugin-opener");
    await opener.openPath(path);
  } else {
    console.info(`[browser] would open ${path}`);
  }
}
