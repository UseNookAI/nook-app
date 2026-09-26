/** DayPartTest.kt */
import { describe, expect, it } from "vitest";
import { dayPartFor, greetingForNow, type DayPart } from "./dayPart";

const at = (hour: number, minute = 0) => new Date(2026, 8, 25, hour, minute);

describe("dayPart", () => {
  it("the icon follows the greeting through the day", () => {
    const expected: [number, DayPart][] = [
      [4, "NIGHT"],
      [5, "SUNRISE"],
      [11, "SUNRISE"],
      [12, "DAY"],
      [16, "DAY"],
      [17, "SUNSET"],
      [19, "SUNSET"],
      [20, "NIGHT"],
      [0, "NIGHT"],
    ];
    for (const [hour, part] of expected) expect(dayPartFor(at(hour, 30)), `at ${hour}:30`).toBe(part);
  });

  it("morning is sun and evening is dusk or moon", () => {
    for (let hour = 0; hour < 24; hour++) {
      const part = dayPartFor(at(hour));
      const greeting = greetingForNow(at(hour));
      if (greeting === "Good morning") expect(part).toBe("SUNRISE");
      else if (greeting === "Good afternoon") expect(part).toBe("DAY");
      else expect(["SUNSET", "NIGHT"]).toContain(part);
    }
  });
});
