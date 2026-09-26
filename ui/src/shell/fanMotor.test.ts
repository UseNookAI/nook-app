/** FanMotorTest.kt */
import { describe, expect, it } from "vitest";
import { FanMotor } from "./fanMotor";

function run(motor: FanMotor, ms: number, throttle: number): FanMotor {
  for (let i = 0; i < Math.floor(ms / 16); i++) motor.step(16, throttle);
  return motor;
}

describe("FanMotor", () => {
  it("spins up to the share of top speed the throttle asks for", () => {
    const full = run(new FanMotor(), 8000, 1);
    expect(Math.abs(full.speed - FanMotor.MAX)).toBeLessThan(0.01);
    expect(full.warmth).toBeCloseTo(1, 3);

    const half = run(new FanMotor(), 20000, 0.5);
    expect(Math.abs(half.speed - FanMotor.MAX / 2)).toBeLessThan(0.02);
    expect(half.warmth).toBeCloseTo(1, 3); // lavender while switched on, whatever the speed

    run(full, 20000, 0.5);
    expect(Math.abs(full.speed - FanMotor.MAX / 2)).toBeLessThan(0.02); // the drag slows it to the lower speed
  });

  it("turns lavender promptly even at a low throttle", () => {
    const motor = run(new FanMotor(), 1600, 0.2);
    expect(motor.warmth).toBeGreaterThan(0.99);
  });

  it("switched off, coasts down and stops upright and green", () => {
    for (const spin of [300, 1234, 5000]) {
      const motor = run(new FanMotor(), spin, 1);
      let ms = 0;
      while (!motor.atRest && ms < 20000) {
        motor.step(16, 0);
        ms += 16;
      }
      expect(motor.atRest).toBe(true);
      expect(motor.angle).toBe(0);
      expect(motor.warmth).toBeLessThan(0.01);
      expect(ms).toBeGreaterThanOrEqual(500); // a run-down, not a snap
      expect(ms).toBeLessThanOrEqual(8000); // or a crawl
    }
  });

  it("switched back on while braking, spins up again", () => {
    const motor = run(new FanMotor(), 3000, 1);
    while (motor.speed > FanMotor.SETTLE) motor.step(16, 0);
    run(motor, 3000, 1);
    expect(motor.speed).toBeGreaterThan(FanMotor.MAX * 0.9);
  });
});
