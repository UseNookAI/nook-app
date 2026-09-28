import { describe, expect, it } from "vitest";
import { reasonText } from "./QuitDialog";
import { leaveReasons, registerLeaveCheck } from "./unsaved";

describe("closing the window", () => {
  it("hears from every area, and a check that fails says nothing", () => {
    registerLeaveCheck("code", () => ["main.rs has changes that are not saved yet"]);
    registerLeaveCheck("broken", () => {
      throw new Error("no");
    });
    registerLeaveCheck("quiet", () => []);
    expect(leaveReasons()).toEqual(["main.rs has changes that are not saved yet"]);
    registerLeaveCheck("code", () => []);
    expect(leaveReasons()).toEqual([]);
  });

  it("says each reason as a sentence", () => {
    expect(reasonText("a PDF has changes that are not saved yet")).toBe("A PDF has changes that are not saved yet");
  });
});
