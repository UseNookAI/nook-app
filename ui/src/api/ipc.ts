/**
 * The one door to the Rust side. `call` invokes a Tauri command (`<area>_<action>`, see
 * src-tauri/src/commands) and `on` listens to a core event topic (`nook:<topic>`).
 *
 * Outside Tauri (plain `npm run dev` in a browser) both fall back to the mocks registered in
 * `./mocks`, so every screen can be looked at and clicked through without the backend.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export const inTauri: boolean = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

type MockHandler = (args: Record<string, unknown>) => unknown | Promise<unknown>;
const mockHandlers = new Map<string, MockHandler>();
const mockListeners = new Map<string, Set<(payload: unknown) => void>>();

/** Registers a browser-only stand-in for a command (see ./mocks). */
export function mock(command: string, handler: MockHandler): void {
  mockHandlers.set(command, handler);
}

/** Lets a mock push an event, as the core would. */
export function mockEmit(topic: string, payload: unknown): void {
  mockListeners.get(topic)?.forEach((fn) => fn(payload));
}

/** Invokes a command. Rejects with the user-facing message (a string) on failure. */
export async function call<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (inTauri) {
    try {
      return await invoke<T>(command, args);
    } catch (e) {
      throw new Error(typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e));
    }
  }
  const handler = mockHandlers.get(command);
  if (!handler) throw new Error(`No mock for ${command}`);
  return (await handler(args)) as T;
}

/** Subscribes to a core event topic ("code", "runtime", "downloads", ...). Returns the unsubscribe. */
export function on<T>(topic: string, handler: (payload: T) => void): () => void {
  if (inTauri) {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    listen<T>(`nook:${topic}`, (e) => handler(e.payload)).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }
  const set = mockListeners.get(topic) ?? new Set();
  set.add(handler as (payload: unknown) => void);
  mockListeners.set(topic, set);
  return () => set.delete(handler as (payload: unknown) => void);
}

/** A message for a caught error, for snackbars. */
export function messageOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
