import { describe, expect, it } from "vitest";
import { usageNoticeDue, usageSentText } from "./usage";

describe("usage", () => {
  it("the notice shows once, and only while the reports are on", () => {
    expect(usageNoticeDue({ SHARE_USAGE: "true", USAGE_NOTICE_SHOWN: "false" })).toBe(true);
    expect(usageNoticeDue({ SHARE_USAGE: "true", USAGE_NOTICE_SHOWN: "true" })).toBe(false);
    expect(usageNoticeDue({ SHARE_USAGE: "false", USAGE_NOTICE_SHOWN: "false" })).toBe(false);
    // An older core without the setting: no notice, and that core sends nothing either.
    expect(usageNoticeDue({})).toBe(false);
  });

  it("says whether this build sends at all, then when it last did", () => {
    const now = new Date(2026, 9, 2, 15, 0);
    expect(usageSentText({ enabled: true, sends: false, lastSent: null }, now)).toMatch(/sends no reports/);
    expect(usageSentText({ enabled: false, sends: true, lastSent: null }, now)).toMatch(/^Nothing, while/);
    expect(usageSentText({ enabled: true, sends: true, lastSent: null }, now)).toMatch(/^Nothing sent yet/);
    expect(usageSentText({ enabled: true, sends: true, lastSent: new Date(2026, 9, 2, 9, 30).toISOString() }, now)).toMatch(
      /^Last sent today at /,
    );
    expect(usageSentText({ enabled: true, sends: true, lastSent: new Date(2026, 8, 28, 9, 30).toISOString() }, now)).toMatch(
      // In the machine's own date order.
      /^Last sent (28 September|September 28)\./,
    );
  });
});
