import { mock } from "../ipc";
import type { UsageOverview } from "../usage";
import { mockFlag, mockSettings } from "./app";

/** `?usage=notice` shows the hub's one-time notice; `?usage=nosend` is a build that sends nothing. */
export function registerUsageMocks(): void {
  if (mockFlag("usage") === "notice") mockSettings.USAGE_NOTICE_SHOWN = "false";
  const sends = mockFlag("usage") !== "nosend";
  const lastSent = new Date(Date.now() - 5 * 3600_000).toISOString();

  const overview = (): UsageOverview => {
    const enabled = mockSettings.SHARE_USAGE === "true";
    return {
      enabled,
      sends,
      lastSent: sends ? lastSent : null,
      next: {
        install: "3f0c9a52-6d1e-4b7a-9c2e-8a41d5b07e19",
        version: "0.5.22",
        os: "Windows",
        channel: mockSettings.UPDATE_CHANNEL === "dev" ? "dev" : "stable",
        gpu: { vendor: "nvidia", vramGb: 8 },
        tools: enabled
          ? {
              "convert.run": { uses: 3, failed: 0 },
              "finder.search": { uses: 5, failed: 0 },
              "pdf.open": { uses: 2, failed: 0 },
              "pdf.save": { uses: 1, failed: 1 },
            }
          : {},
      },
    };
  };
  mock("usage_overview", overview);
  mock("usage_set", ({ enabled }) => {
    mockSettings.SHARE_USAGE = String(Boolean(enabled));
  });
  mock("usage_notice_seen", () => {
    mockSettings.USAGE_NOTICE_SHOWN = "true";
  });
}
