/**
 * The Code snapshot and actions every Code screen shares (CodeHub.kt's `read(service)` and
 * CodeActions): one subscription to "code" events re-reads `codeSnapshot()` for all of them (and
 * "downloads" events, so a model that finishes downloading joins the worker menu), and actions
 * show what went wrong in the snackbar.
 */
import { useCallback, useMemo, useSyncExternalStore } from "react";
import { codeSnapshot, onCodeChanged, onDownloadsChanged, type CodeSession, type CodeSnapshot, type Run } from "../../api/code";
import { messageOf } from "../../api/ipc";
import { useSnackbar } from "../../components/Snackbar";

const EMPTY: CodeSnapshot = { sessions: [], workerName: null, workerHint: "a worker model", workers: [], workerId: null, phases: {} };

let current: CodeSnapshot = EMPTY;
const listeners = new Set<() => void>();
let unsubscribe: (() => void) | null = null;
let reading = false;
let again = false;

/** Reads the snapshot; a change that lands while a read is in flight reads once more after it. */
function refresh(): void {
  if (reading) {
    again = true;
    return;
  }
  reading = true;
  codeSnapshot()
    .then((s) => {
      current = s;
      listeners.forEach((l) => l());
    })
    .catch((e) => console.warn("Could not read the Code snapshot:", messageOf(e)))
    .finally(() => {
      reading = false;
      if (again) {
        again = false;
        refresh();
      }
    });
}

/** How long download events settle before the snapshot is read again: they come with every bit of progress. */
const DOWNLOADS_SETTLE_MS = 1000;

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  if (!unsubscribe) {
    // Changes come from the worker; the snapshot is replaced as a whole.
    const offCode = onCodeChanged(refresh);
    let timer: number | undefined;
    const offDownloads = onDownloadsChanged(() => {
      if (timer === undefined) {
        timer = window.setTimeout(() => {
          timer = undefined;
          refresh();
        }, DOWNLOADS_SETTLE_MS);
      }
    });
    unsubscribe = () => {
      offCode();
      offDownloads();
      window.clearTimeout(timer);
    };
    refresh();
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0 && unsubscribe) {
      unsubscribe();
      unsubscribe = null;
    }
  };
}

/** The current snapshot, kept up to date while mounted. */
export function useCodeSnapshot(): CodeSnapshot {
  return useSyncExternalStore(subscribe, () => current);
}

/** Runs a command and shows what went wrong in the snackbar (CodeActions.run / say). */
export function useCodeActions() {
  const { say } = useSnackbar();
  const run = useCallback(
    (block: () => Promise<unknown>) => {
      block().catch((e) => say(messageOf(e)));
    },
    [say],
  );
  return useMemo(() => ({ run, say }), [run, say]);
}

export type CodeActions = ReturnType<typeof useCodeActions>;

// ------------------------------------------------------------------ CodeSession's helpers

/** The last run, or null. */
export function lastRun(s: CodeSession): Run | null {
  for (let i = s.entries.length - 1; i >= 0; i--) {
    const e = s.entries[i];
    if (e.kind === "run") return e;
  }
  return null;
}

export function isRunning(s: CodeSession): boolean {
  return lastRun(s)?.running === true;
}

/**
 * The run Undo would take back: the last one, when it finished, is not undone yet, knows its
 * starting tree, and nothing was applied or discarded since.
 */
export function undoable(s: CodeSession): Run | null {
  for (let i = s.entries.length - 1; i >= 0; i--) {
    const e = s.entries[i];
    if (e.kind === "note" && (e.tone === "applied" || e.tone === "discarded")) return null;
    if (e.kind === "run") return !e.running && !e.undone && e.before != null ? e : null;
  }
  return null;
}

export const capitalise = (s: string) => (s ? s.charAt(0).toUpperCase() + s.slice(1) : s);

/** "12 s", "3 min 4 s". */
export function duration(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  return s < 60 ? `${s} s` : `${Math.floor(s / 60)} min ${s % 60} s`;
}

/** "HH:mm" in local time. */
export function time(at: number): string {
  const d = new Date(at);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}
