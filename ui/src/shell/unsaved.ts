/**
 * Work the window would lose if it closed now, held in the UI itself (the Code page's edited
 * files). Each area registers once, when its module loads, and the check lives as long as the
 * app, whether or not its page is on screen, so closing the window from anywhere asks about it.
 * What the core holds (an edited PDF, a running Nooklet, a render) the core says itself
 * (`quitCheck` in ../api/app).
 *
 *   registerLeaveCheck("code", () => workspace ? workspace.dirtyTabs().map((t) => `${t.name} is not saved`) : []);
 */

type Check = () => string[];

const checks = new Map<string, Check>();

/** Registers (or replaces) the check named `name`. */
export function registerLeaveCheck(name: string, check: Check): void {
  checks.set(name, check);
}

/** What every check says would be lost; a check that fails says nothing. */
export function leaveReasons(): string[] {
  const out: string[] = [];
  for (const check of checks.values()) {
    try {
      out.push(...check());
    } catch {
      // A check that cannot answer does not keep the window open.
    }
  }
  return out;
}
