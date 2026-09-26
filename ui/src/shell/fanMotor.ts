/**
 * The coming-soon page's fan (Frontend/Coming Soon, startFan), driven by the GPU: a motor against
 * the air (FanMark.kt's FanMotor). The throttle sets the torque, and the drag grows with the square
 * of the speed, so it spins up quick at first and eases into the speed where the two meet: MAX times
 * the throttle. A lower throttle lets the drag slow it to the new speed. Switched off, it coasts down
 * against the drag and a little friction, then over its last half turn or so brakes evenly to stop
 * upright, so it ends as the still mark it started as. Angles in degrees, speeds in degrees per ms.
 */
export class FanMotor {
  static readonly MAX = 3.4; // about 9 turns a second
  static readonly TAU = 1100; // ms
  static readonly FRICTION = 0.0008; // deg/ms²
  static readonly SETTLE = 0.25; // the speed below which it picks the upright to stop at

  speed = 0;
  angle = 0;
  /** 0 green, 1 lavender: warm with the speed as on the site, and warm at once while switched on. */
  warmth = 0;
  private switchedOn = 0; // eases to 1 while the throttle is open, and back to 0 after
  private throttle = 0;
  private rest = Number.NaN; // the upright it is braking to
  private brake = 0;

  get atRest(): boolean {
    return this.throttle === 0 && this.speed === 0 && this.angle === 0 && Number.isNaN(this.rest) && this.switchedOn === 0;
  }

  step(dtMs: number, throttle: number): void {
    const { MAX, TAU, FRICTION, SETTLE } = FanMotor;
    this.throttle = Math.max(0, Math.min(1, throttle));
    const drag = (MAX / TAU) * (this.speed / MAX) * (this.speed / MAX);
    if (this.throttle > 0) {
      this.speed = Math.max(0, Math.min(MAX, this.speed + ((MAX / TAU) * this.throttle * this.throttle - drag) * dtMs));
      this.rest = Number.NaN;
    } else if (Number.isNaN(this.rest)) {
      this.speed = Math.max(0, this.speed - (drag + FRICTION) * dtMs);
      if (this.speed <= SETTLE) {
        // The N looks the same turned half way round, so any multiple of 180 degrees is upright.
        this.angle %= 180;
        this.rest = Math.ceil((this.angle + 90) / 180) * 180;
        this.brake = (this.speed * this.speed) / (2 * (this.rest - this.angle));
      }
    } else if (this.speed > 0) {
      this.speed = Math.max(0, this.speed - this.brake * dtMs);
    } else {
      this.angle += (this.rest - this.angle) * Math.min(1, dtMs / 220); // the last hair of the turn, eased in
    }
    this.angle += this.speed * dtMs;
    const on = this.throttle > 0 ? 1 : 0;
    this.switchedOn += (on - this.switchedOn) * Math.min(1, dtMs / 300);
    if (on === 0 && this.switchedOn < 0.005) this.switchedOn = 0;
    if (!Number.isNaN(this.rest) && (this.angle >= this.rest || (this.speed === 0 && this.rest - this.angle <= 0.05))) {
      this.speed = 0;
      this.angle = 0;
      this.rest = Number.NaN;
    } else if (Number.isNaN(this.rest)) {
      this.angle %= 180;
    }
    const fraction = 1 - this.speed / MAX;
    this.warmth = Math.max(this.switchedOn, 1 - fraction * fraction);
  }
}
