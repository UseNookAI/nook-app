/** The time-of-day logic of Greeting.kt and TimeOfDayIcon.kt, apart from the drawing. */
import { useEffect, useState } from "react";

/** The part of the day the greeting's icon shows. */
export type DayPart = "SUNRISE" | "DAY" | "SUNSET" | "NIGHT";

/** Returns "Good morning", "Good afternoon" or "Good evening" for the local clock. */
export function greetingForNow(now: Date = new Date()): string {
  const h = now.getHours();
  if (h >= 5 && h <= 11) return "Good morning";
  if (h >= 12 && h <= 16) return "Good afternoon";
  return "Good evening";
}

/** Morning is a sunrise, the afternoon a full sun, early evening a sunset, and after that the moon. */
export function dayPartFor(now: Date): DayPart {
  const h = now.getHours();
  if (h >= 5 && h <= 11) return "SUNRISE";
  if (h >= 12 && h <= 16) return "DAY";
  if (h >= 17 && h <= 19) return "SUNSET";
  return "NIGHT";
}

/** The local time, read again on every minute, so a page left open keeps up with the day. */
export function useLocalTime(): Date {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    let timer: number;
    const next = () => {
      timer = window.setTimeout(() => {
        setNow(new Date());
        next();
      }, 60_000 - (Date.now() % 60_000));
    };
    next();
    return () => window.clearTimeout(timer);
  }, []);
  return now;
}
