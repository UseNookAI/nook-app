import { mock, mockEmit } from "../ipc";

/**
 * Browser-preview switches in the address bar, e.g. http://localhost:1430/?welcome&dark:
 * `welcome` (first run), `dark` / `system` (theme), `update` (an update is on offer), `engine`
 * (the hub installs the runtime; `engine=fail` fails), `gpu` (the brand mark spins).
 * The shell adds `settings=<tab id>` (Settings open at start, e.g. settings=models/code) and
 * `gallery` (every shared component in place of the hub).
 */
export function mockFlag(name: string): string | null {
  if (typeof window === "undefined") return null;
  return new URLSearchParams(window.location.search).get(name);
}

const DEFAULTS: Record<string, string> = {
  IS_EULA_ACCEPTED: "false",
  IS_SETUP_COMPLETED: "false",
  SETUP_VERSION: "0",
  USAGE_PREFERENCES: "",
  APP_THEME: "Light",
  IS_ADVANCED_MODE: "false",
  LAST_ACTIVE_SCREEN: "",
  UPDATE_CHANNEL: "stable",
};

const settings: Record<string, string> = {
  ...DEFAULTS,
  IS_SETUP_COMPLETED: "true",
  SETUP_VERSION: "1",
};

const document = (title: string, body: string) =>
  `<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>${title}</title></head>` +
  `<body style="margin:0;background:#fff;"><div style="max-width:820px;margin:0 auto;padding:32px 28px;` +
  `font-family:Segoe UI,Roboto,Helvetica,Arial,sans-serif;line-height:1.55;color:#1B1A18;">` +
  `<h1 style="font-size:28px;margin:0 0 12px;font-weight:600;">${title}</h1>${body}</div></body></html>`;

export function registerAppMocks(): void {
  if (mockFlag("welcome") !== null) settings.IS_SETUP_COMPLETED = "false";
  if (mockFlag("dark") !== null) settings.APP_THEME = "Dark";
  if (mockFlag("system") !== null) settings.APP_THEME = "System";

  mock("app_info", () => ({
    build: { version: "0.5.0", commit: "mock", time: new Date().toISOString() },
    label: "0.5.0 (mock, 2026-09-25)",
    home: "C:\\Users\\you\\AppData\\Local\\Nook-rs",
  }));
  mock("settings_all", () => ({ ...settings }));
  mock("settings_set", ({ name, value }) => {
    settings[name as string] = value as string;
    mockEmit("settings", { name });
  });
  mock("app_erase_everything", async () => {
    await new Promise((r) => setTimeout(r, 900));
    for (const key of Object.keys(settings)) delete settings[key];
    Object.assign(settings, DEFAULTS);
  });
  mock("app_quit", () => console.info("[browser] app_quit: the app would exit here"));
  mock("app_eula", () =>
    document(
      "Nook software licence",
      "<p>The browser preview shows a stand-in. Inside the app this is resources/eula.html.</p>",
    ),
  );
  mock("app_notices", () =>
    document(
      "Third-party notices",
      "<p>The browser preview shows a stand-in. Inside the app this is resources/third-party-notices.html.</p>",
    ),
  );
}
