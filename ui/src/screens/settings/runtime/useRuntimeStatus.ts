/**
 * RuntimeManager.status(), read while a page is open: every `intervalMs` as the Kotlin pages polled
 * it (Runtime every two seconds, the Library every three), and at once on each "runtime" event.
 * Null until the first read; a failed read keeps the last one (runCatching { ... }.getOrNull()
 * cleared it, which blanked the page for a poll).
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { onRuntime, runtimeStatus, type RuntimeStatus } from "../../../api/runtime";

export function useRuntimeStatus(intervalMs: number): [RuntimeStatus | null, () => void] {
  const [status, setStatus] = useState<RuntimeStatus | null>(null);
  const readRef = useRef<() => void>(() => {});

  useEffect(() => {
    let alive = true;
    let reading = false;
    let again = false;
    const read = () => {
      if (reading) {
        again = true;
        return;
      }
      reading = true;
      runtimeStatus()
        .then((s) => alive && setStatus(s))
        .catch(() => undefined)
        .finally(() => {
          reading = false;
          if (again && alive) {
            again = false;
            read();
          }
        });
    };
    readRef.current = read;
    read();
    const timer = window.setInterval(read, intervalMs);
    const off = onRuntime(() => read());
    return () => {
      alive = false;
      window.clearInterval(timer);
      off();
    };
  }, [intervalMs]);

  const refresh = useCallback(() => readRef.current(), []);
  return [status, refresh];
}
