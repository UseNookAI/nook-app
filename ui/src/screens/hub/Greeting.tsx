/**
 * The empty chat state (Greeting.kt): the sun or moon, a time-of-day greeting and one quiet line
 * under it. The Chat start page (CodeStartScreen) uses it.
 */
import { dayPartFor, greetingForNow, useLocalTime } from "./dayPart";
import { TimeOfDayIcon } from "./TimeOfDayIcon";
import "./hub.css";

export function Greeting({ subtitle = "What's on your mind?" }: { subtitle?: string }) {
  const now = useLocalTime();
  return (
    <div className="nk-greeting">
      <div className="nk-greeting__row">
        <TimeOfDayIcon part={dayPartFor(now)} size={32} />
        <span className="h2 nk-greeting__title">{greetingForNow(now)}.</span>
      </div>
      <div className="body-large nk-greeting__subtitle">{subtitle}</div>
    </div>
  );
}
