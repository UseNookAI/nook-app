/**
 * Browser stand-ins for the code-session commands (nook_core::code) and voice input
 * (nook_core::speech): six sessions that show every state of the thread, a run whose steps grow
 * on a timer, and start / send / stop / apply / discard / undo / delete / rename / set-worker /
 * speech mutating that state the way CodeService.java does, with its messages.
 */
import type { Change, CodeSession, CodeSnapshot, Entry, NextContext, RepositoryState, Run, RunContext, SpeechDownload, SpeechModel } from "../code";
import { THINKING } from "../code";
import { mock, mockEmit } from "../ipc";

const HOME = "C:\\Users\\you\\Projects";
const MINUTE = 60_000;
const WINDOW = 32_768;

const workers = [
  { id: "gpt-oss-20b", name: "gpt-oss 20B", tested: true },
  { id: "qwen3-coder-30b-a3b", name: "Qwen3-Coder 30B-A3B", tested: true },
  { id: "qwen3-8b-q4km", name: "Qwen3 8B", tested: false },
  { id: "gemma-3-12b", name: "Gemma 3 12B", tested: false },
];
const workerHint = "gpt-oss 20B or Qwen3-Coder 30B-A3B";
let workerId: string | null = "gpt-oss-20b";
const workerName = () => workers.find((w) => w.id === workerId)?.name ?? null;

const sessions = new Map<string, CodeSession>();
const phases: Record<string, string> = {};
/** The code each run wrote (CodeService.runDiff), by run id. */
const runDiffs = new Map<string, string>();
/** The change a session held before each run, for Undo. */
const changeBefore = new Map<string, Change | null>();
let counter = 0;
const newId = () => (++counter).toString(16).padStart(8, "0").slice(-8);

const emit = () => mockEmit("code", {});
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// ------------------------------------------------------------------ diffs

const DIFF_DATES_1 = `diff --git a/src/main/java/ledger/DateParser.java b/src/main/java/ledger/DateParser.java
index 3c1d2e0..9a7f1b2 100644
--- a/src/main/java/ledger/DateParser.java
+++ b/src/main/java/ledger/DateParser.java
@@ -12,11 +12,17 @@ import java.time.format.DateTimeFormatter;
 public final class DateParser {
-    public DateParser() { }
+    private final Clock clock;
+
+    public DateParser() { this(Clock.systemUTC()); }
+
+    public DateParser(Clock clock) { this.clock = clock; }

     public LocalDate parse(String text) {
         if ("today".equalsIgnoreCase(text.strip())) {
-            return LocalDate.now();
+            return LocalDate.now(clock);
         }
         return LocalDate.parse(text.strip(), DateTimeFormatter.ISO_LOCAL_DATE);
     }
diff --git a/src/test/java/ledger/DateParserTest.java b/src/test/java/ledger/DateParserTest.java
index 88a01f4..51cc0d9 100644
--- a/src/test/java/ledger/DateParserTest.java
+++ b/src/test/java/ledger/DateParserTest.java
@@ -8,9 +8,13 @@ class DateParserTest {
-    private final DateParser parser = new DateParser();
+    private static final Clock FIXED = Clock.fixed(Instant.parse("2026-03-14T23:30:00Z"), ZoneOffset.UTC);
+    private final DateParser parser = new DateParser(FIXED);

     @Test
     void today() {
-        assertEquals(LocalDate.now(), parser.parse("today"));
+        assertEquals(LocalDate.of(2026, 3, 14), parser.parse("today"));
     }
`;

const newJavaFile = (() => {
  const body = [
    "package ledger;",
    "",
    "import java.time.format.DateTimeFormatter;",
    "import java.util.List;",
    "",
    "/** The date formats the parser accepts, tried in order. */",
    "final class DateFormats {",
    "    private DateFormats() { }",
    "",
    "    static final List<DateTimeFormatter> ACCEPTED = List.of(",
    "            DateTimeFormatter.ISO_LOCAL_DATE,",
    '            DateTimeFormatter.ofPattern("dd.MM.uuuu"),',
    '            DateTimeFormatter.ofPattern("d.M.uuuu"),',
    '            DateTimeFormatter.ofPattern("dd/MM/uuuu"));',
    "",
    "    /** Whether a two-digit year should be read as this century. */",
    "    static boolean shortYear(String text) {",
    "        int dot = text.lastIndexOf('.');",
    "        return dot >= 0 && text.length() - dot - 1 == 2;",
    "    }",
    "",
    "    static String describe() {",
    "        StringBuilder sb = new StringBuilder();",
    "        for (DateTimeFormatter f : ACCEPTED) {",
    "            if (sb.length() > 0) sb.append(\", \");",
    "            sb.append(f.toString());",
    "        }",
    "        return sb.toString();",
    "    }",
    "",
    "    static final String[] EXAMPLES = {",
    '            "2026-03-14",',
    '            "14.03.2026",',
    '            "4.3.2026",',
    '            "14/03/2026",',
    "    };",
    "",
    "    static int count() {",
    "        return ACCEPTED.size();",
    "    }",
    "}",
  ];
  return (
    "diff --git a/src/main/java/ledger/DateFormats.java b/src/main/java/ledger/DateFormats.java\n" +
    "new file mode 100644\nindex 0000000..4b1e2aa\n--- /dev/null\n+++ b/src/main/java/ledger/DateFormats.java\n" +
    `@@ -0,0 +1,${body.length} @@\n` +
    body.map((l) => "+" + l).join("\n") +
    "\n"
  );
})();

const DIFF_DATES_2 =
  `diff --git a/src/main/java/ledger/DateParser.java b/src/main/java/ledger/DateParser.java
index 9a7f1b2..c04d7e1 100644
--- a/src/main/java/ledger/DateParser.java
+++ b/src/main/java/ledger/DateParser.java
@@ -22,7 +22,15 @@ public final class DateParser {
         if ("today".equalsIgnoreCase(text.strip())) {
             return LocalDate.now(clock);
         }
-        return LocalDate.parse(text.strip(), DateTimeFormatter.ISO_LOCAL_DATE);
+        String t = text.strip();
+        for (DateTimeFormatter f : DateFormats.ACCEPTED) {
+            try {
+                return LocalDate.parse(t, f);
+            } catch (DateTimeParseException ignored) {
+                // the next format
+            }
+        }
+        throw new IllegalArgumentException("Not a date: " + t);
     }
 }
` +
  newJavaFile +
  `diff --git a/src/test/java/ledger/DateParserTest.java b/src/test/java/ledger/DateParserTest.java
index 51cc0d9..e2f9a10 100644
--- a/src/test/java/ledger/DateParserTest.java
+++ b/src/test/java/ledger/DateParserTest.java
@@ -18,4 +18,19 @@ class DateParserTest {
         assertEquals(LocalDate.of(2026, 3, 14), parser.parse("today"));
     }
+
+    @Test
+    void dottedDates() {
+        assertEquals(LocalDate.of(2026, 3, 14), parser.parse("14.03.2026"));
+    }
+
+    @Test
+    void leapDay() {
+        assertEquals(LocalDate.of(2028, 2, 29), parser.parse("29.02.2028"));
+    }
+
+    @Test
+    void notALeapYear() {
+        assertThrows(IllegalArgumentException.class, () -> parser.parse("29.02.2027"));
+    }
 }
diff --git a/src/main/java/ledger/LegacyDates.java b/src/main/java/ledger/LegacyDates.java
deleted file mode 100644
index 7d1c3b4..0000000
--- a/src/main/java/ledger/LegacyDates.java
+++ /dev/null
@@ -1,9 +0,0 @@
-package ledger;
-
-/** Replaced by DateFormats. */
-@Deprecated
-final class LegacyDates {
-    static boolean dotted(String s) {
-        return s.matches("\\\\d{2}\\\\.\\\\d{2}\\\\.\\\\d{4}");
-    }
-}
`;

const VERIFY_OUTPUT = `> Task :test

DateParserTest > notALeapYear() FAILED
    org.opentest4j.AssertionFailedError: Expected java.lang.IllegalArgumentException to be thrown, but nothing was thrown.
        at app//org.junit.jupiter.api.AssertionFailureBuilder.build(AssertionFailureBuilder.java:152)
        at app//org.junit.jupiter.api.AssertThrows.assertThrows(AssertThrows.java:73)
        at app//ledger.DateParserTest.notALeapYear(DateParserTest.java:34)

    java.time.format.DateTimeParseException: Text '29.02.2027' could not be parsed: Invalid date 'February 29' as '2027' is not a leap year
        (resolved leniently by ResolverStyle.SMART to 2027-02-28)

12 tests completed, 1 failed

FAILURE: Build failed with an exception.

* What went wrong:
Execution failed for task ':test'.
> There were failing tests. See the report at: file:///C:/Users/you/Projects/ledger-api/build/reports/tests/test/index.html

BUILD FAILED in 14s`;

const DIFF_CONFIG = `diff --git a/src/config.rs b/src/config.rs
index 1a2b3c4..5d6e7f8 100644
--- a/src/config.rs
+++ b/src/config.rs
@@ -1,12 +1,12 @@
-pub struct ConfigLoader {
+pub struct Settings {
     path: PathBuf,
 }

-impl ConfigLoader {
+impl Settings {
     pub fn open(path: impl Into<PathBuf>) -> Self {
         Self { path: path.into() }
     }
diff --git a/src/main.rs b/src/main.rs
index 0f0e0d0..a1a2a3a 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -3,7 +3,7 @@ mod config;
 fn main() -> anyhow::Result<()> {
-    let config = config::ConfigLoader::open("nook.toml");
+    let config = config::Settings::open("nook.toml");
     run(config)
 }
`;

const DIFF_SPLIT = `diff --git a/src/config/mod.rs b/src/config/mod.rs
new file mode 100644
index 0000000..b0b1b2b
--- /dev/null
+++ b/src/config/mod.rs
@@ -0,0 +1,4 @@
+mod json;
+mod toml;
+
+pub use self::toml::Settings;
`;

const DIFF_CSV = `diff --git a/src/export/csv.ts b/src/export/csv.ts
index 4e4e4e4..5f5f5f5 100644
--- a/src/export/csv.ts
+++ b/src/export/csv.ts
@@ -1,22 +1,31 @@
-import { writeFileSync } from "node:fs";
+import { createWriteStream } from "node:fs";
+import { Readable, Transform } from "node:stream";
+import { pipeline } from "node:stream/promises";

-export function exportCsv(rows: Row[], file: string): void {
-  const lines = [header(rows[0])];
-  for (const row of rows) lines.push(line(row));
-  writeFileSync(file, lines.join("\\n"));
+export async function exportCsv(rows: AsyncIterable<Row>, file: string): Promise<void> {
+  let first = true;
+  const toLines = new Transform({
+    objectMode: true,
+    transform(row: Row, _enc, done) {
+      const out = first ? header(row) + "\\n" + line(row) + "\\n" : line(row) + "\\n";
+      first = false;
+      done(null, out);
+    },
+  });
+  await pipeline(Readable.from(rows), toLines, createWriteStream(file));
 }
`;

const DIFF_SITE = (file: string, text: string) => `diff --git a/${file} b/${file}
index 1111111..2222222 100644
--- a/${file}
+++ b/${file}
@@ -8,6 +8,9 @@ export function Settings() {
   return (
     <section className="settings">
       <h2>Settings</h2>
+      {/* ${text} */}
+      <ThemeToggle />
+      <p className="hint">Follows the system until you choose.</p>
       <Language />
     </section>
   );
`;

const DIFF_EDIT = (file: string, text: string) => `diff --git a/${file} b/${file}
index 3333333..4444444 100644
--- a/${file}
+++ b/${file}
@@ -40,4 +40,7 @@
         }
     }

+    // ${text}
+    // (what the mock worker wrote for this request)
+
 }
`;

// ------------------------------------------------------------------ helpers

const run =(fields: Partial<Run> & { id: string; at: number }): Run => ({
  kind: "run",
  model: workerName(),
  running: false,
  steps: [],
  summary: null,
  stat: null,
  verified: null,
  verifyCommand: null,
  verifyNote: null,
  verifyOutput: null,
  gaveUp: null,
  toolCalls: 0,
  seconds: 0,
  error: null,
  before: null,
  undone: false,
  after: null,
  context: null,
  ...fields,
});

const ctx = (peak: number, dropped = 0, measured = true, used = peak): RunContext => ({ used, peak, window: WINDOW, dropped, measured });

/** "3 files changed, 40 insertions(+), 2 deletions(-)" for a diff, as `git diff --stat` ends. */
function statOf(diff: string): string {
  const files = (diff.match(/^diff --git /gm) ?? []).length;
  let plus = 0;
  let minus = 0;
  for (const l of diff.split("\n")) {
    if (l.startsWith("+") && !l.startsWith("+++")) plus++;
    else if (l.startsWith("-") && !l.startsWith("---")) minus++;
  }
  const parts = [`${files} ${files === 1 ? "file" : "files"} changed`];
  if (plus > 0) parts.push(`${plus} insertion${plus === 1 ? "" : "s"}(+)`);
  if (minus > 0) parts.push(`${minus} deletion${minus === 1 ? "" : "s"}(-)`);
  return " src | 0\n " + parts.join(", ");
}

const changeOf = (diff: string | null): Change | null => (diff && diff.trim() ? { diff, cut: false, stat: statOf(diff) } : null);

function titleFor(text: string): string {
  let t = text.trim();
  const nl = t.indexOf("\n");
  if (nl >= 0) t = t.substring(0, nl).trim();
  return t === "" ? "New session" : t.length > 60 ? t.substring(0, 57) + "..." : t;
}

const note = (text: string, tone: string | null, at = Date.now()): Entry => ({ kind: "note", id: newId(), at, text, tone });
const task = (text: string, at: number): Entry => ({ kind: "task", id: newId(), at, text });

function put(s: CodeSession, touch = true): void {
  sessions.set(s.id, touch ? { ...s, updatedAt: Date.now() } : s);
}

function editRun(sid: string, rid: string, edit: (r: Run) => Run, touch = true): void {
  const s = sessions.get(sid);
  if (!s) return;
  put({ ...s, entries: s.entries.map((e) => (e.kind === "run" && e.id === rid ? edit(e) : e)) }, touch);
}

const lastRunOf = (s: CodeSession): Run | null => {
  for (let i = s.entries.length - 1; i >= 0; i--) {
    const e = s.entries[i];
    if (e.kind === "run") return e;
  }
  return null;
};
const runningOf = (s: CodeSession) => lastRunOf(s)?.running === true;

function undoableOf(s: CodeSession): Run | null {
  for (let i = s.entries.length - 1; i >= 0; i--) {
    const e = s.entries[i];
    if (e.kind === "note" && (e.tone === "applied" || e.tone === "discarded")) return null;
    if (e.kind === "run") return !e.running && !e.undone && e.before != null ? e : null;
  }
  return null;
}

function require(id: unknown): CodeSession {
  const s = sessions.get(id as string);
  if (!s) throw new Error("That session is gone.");
  return s;
}

// ------------------------------------------------------------------ the running worker

interface Sim {
  timer: number | undefined;
  finish: (stopped: boolean) => void;
  runId: string;
}
const sims = new Map<string, Sim>();

/** Plays [script] as the worker's steps, a phase of thinking between them, then [finish]. */
function simulate(sid: string, rid: string, script: string[], stepMs: number, finish: (stopped: boolean) => void): void {
  let i = 0;
  const sim: Sim = { timer: undefined, finish, runId: rid };
  sims.set(sid, sim);
  const end = (stopped: boolean) => {
    sims.delete(sid);
    delete phases[rid];
    if (sessions.has(sid)) finish(stopped);
    emit();
  };
  sim.finish = end;
  const tick = () => {
    if (!sessions.has(sid)) {
      sims.delete(sid);
      delete phases[rid];
      return;
    }
    if (i >= script.length) {
      end(false);
      return;
    }
    const step = script[i++];
    editRun(
      sid,
      rid,
      (r) => {
        // The context grows with what the worker reads; near the limit old outputs are dropped.
        const c = r.context;
        let used = (c?.used ?? 3200) + 700 + Math.round(Math.random() * 1400);
        let dropped = c?.dropped ?? 0;
        if (used > WINDOW * 0.86) {
          used -= 6000;
          dropped++;
        }
        const steps = [...r.steps, step];
        return {
          ...r,
          steps: steps.length > 300 ? steps.slice(steps.length - 300) : steps,
          context: { used, peak: Math.max(c?.peak ?? 0, used), window: WINDOW, dropped, measured: true },
        };
      },
    );
    phases[rid] = step;
    emit();
    sim.timer = window.setTimeout(() => {
      if (!sessions.has(sid)) return;
      phases[rid] = THINKING;
      emit();
      sim.timer = window.setTimeout(tick, stepMs * 0.6);
    }, stepMs * 0.4);
  };
  phases[rid] = THINKING;
  sim.timer = window.setTimeout(tick, 900);
}

function stopSim(sid: string): void {
  const sim = sims.get(sid);
  if (!sim) return;
  window.clearTimeout(sim.timer);
  sim.finish(true);
}

/** What a mock turn does for [text] in [s]: its steps, and the file it touches. */
function scriptFor(s: CodeSession, text: string): { steps: string[]; file: string } {
  const repo = s.repository.split("\\").pop() ?? "repo";
  const word = (text.match(/[A-Za-z]{5,}/) ?? ["settings"])[0].toLowerCase();
  const file = repo === "ledger-api" ? "src/main/java/ledger/DateParser.java" : repo === "nook-cli" ? "src/config.rs" : "src/pages/Settings.tsx";
  const steps = [
    ...(s.worktree == null ? [`making a scratch copy of ${repo}`] : []),
    "listing .",
    `searching for ${word}`,
    `reading ${file}`,
    `reading ${file} from line 120`,
    `editing ${file}`,
    repo === "ledger-api" ? "running gradle test" : repo === "nook-cli" ? "running cargo test" : "running npm test",
    `reading ${file}`,
    `editing ${file}`,
  ];
  return { steps, file };
}

/** A greeting or a question about the assistant gets an answer, not a run. */
const isTalk = (text: string) => /^\s*(hi|hello|hey|thanks|thank you|what can you do|who are you)\b/i.test(text);

function send(s: CodeSession, text: string, verify: string | null): void {
  if (runningOf(s)) throw new Error("The worker is still busy with the last request. Stop it first or wait.");
  const t = text.trim();
  if (!t) throw new Error("Say what to change.");
  const name = workerName();
  if (name == null) throw new Error(`No worker model is installed. Download ${workerHint} in Settings > Models.`);
  const rid = newId();
  const started = Date.now();
  changeBefore.set(rid, s.change);
  put({ ...s, verify, entries: [...s.entries, task(t, started), run({ id: rid, at: started, model: name, running: true })] });
  emit();

  if (isTalk(t)) {
    const finish = (stopped: boolean) => {
      sims.delete(s.id);
      delete phases[rid];
      editRun(s.id, rid, (r) => ({
        ...r,
        running: false,
        summary: stopped
          ? null
          : "Hi! I'm the local model in Nook Code. Tell me what to change in this folder: I work on a private copy, " +
            "you read the diff, and nothing reaches your files until you press Apply.",
        gaveUp: stopped ? "stopped" : null,
        seconds: Math.round((Date.now() - started) / 1000),
      }));
      emit();
    };
    phases[rid] = THINKING;
    sims.set(s.id, { timer: window.setTimeout(() => finish(false), 2200), runId: rid, finish });
    return;
  }

  const { steps, file } = scriptFor(s, t);
  const before = "tree-" + newId();
  editRun(s.id, rid, (r) => ({ ...r, before }), false);
  if (s.worktree == null) put({ ...sessions.get(s.id)!, worktree: `C:\\Users\\you\\AppData\\Local\\Temp\\nook-rs\\code\\${s.id}`, baseCommit: "a1b2c3d" }, false);
  simulate(s.id, rid, steps, 1500, (stopped) => {
    const cur = sessions.get(s.id);
    if (!cur) return;
    const wrote = file.endsWith(".tsx") ? DIFF_SITE(file, t.slice(0, 60)) : DIFF_EDIT(file, t.slice(0, 60));
    const diff = (cur.change?.diff ?? "") + wrote;
    const change = changeOf(diff);
    const r = cur.entries.find((e) => e.kind === "run" && e.id === rid) as Run;
    const after = "tree-" + newId();
    runDiffs.set(rid, wrote);
    const checked = verify != null || /test|check/i.test(t);
    put({
      ...cur,
      change,
      entries: cur.entries.map((e) =>
        e.kind === "run" && e.id === rid
          ? {
              ...r,
              running: false,
              summary: stopped
                ? null
                : `I added the change you asked for in ${file.split("/").pop()}: a ThemeToggle in the settings section with a hint under it. ` +
                  "It follows the system theme until the person picks one.",
              stat: change?.stat ?? null,
              verified: stopped || !checked ? null : true,
              verifyCommand: stopped || !checked ? null : (verify ?? "npm test"),
              gaveUp: stopped ? "stopped" : null,
              toolCalls: r.steps.length,
              seconds: Math.round((Date.now() - r.at) / 1000),
              after,
            }
          : e,
      ),
    });
  });
}

// ------------------------------------------------------------------ the sample sessions

function seed(): void {
  const now = Date.now();

  // 1. A run at work: its steps grow on a timer, thinking between them.
  {
    const id = "s-running";
    const rid = "r-running";
    const at = now - 95_000;
    changeBefore.set(rid, null);
    sessions.set(id, {
      id,
      title: "Add a dark mode toggle to the settings page",
      repository: `${HOME}\\nook-site`,
      createdAt: at,
      updatedAt: now,
      worktree: `C:\\Users\\you\\AppData\\Local\\Temp\\nook-rs\\code\\${id}`,
      baseCommit: "9f8e7d6",
      baseline: null,
      verify: null,
      change: null,
      entries: [
        task("Add a dark mode toggle to the settings page. It should follow the system theme until the person picks one, and remember the choice.", at),
        run({
          id: rid,
          at: at + 400,
          model: "gpt-oss 20B",
          running: true,
          before: "tree-r0",
          steps: ["loading gpt-oss 20B on the GPU", "listing .", "listing src", "searching for theme", "reading src/pages/Settings.tsx"],
          context: ctx(9800, 0, true),
        }),
      ],
    });
    const files = ["src/theme.ts", "src/pages/Settings.tsx", "src/components/ThemeToggle.tsx", "src/App.tsx", "src/styles/tokens.css"];
    const script: string[] = [];
    for (let i = 0; i < 40; i++) {
      const f = files[i % files.length];
      script.push(
        [`reading ${f}`, `reading ${f} from line ${40 + i * 7}`, `searching for prefers-color-scheme`, `editing ${f}`, `writing ${f}`, "running npm test", "running npm run typecheck"][i % 7],
      );
    }
    simulate(id, rid, script, 2500, (stopped) => {
      const cur = sessions.get(id);
      if (!cur) return;
      const diff = DIFF_SITE("src/pages/Settings.tsx", "Dark mode");
      runDiffs.set(rid, diff);
      const change = changeOf(diff);
      put({
        ...cur,
        change,
        entries: cur.entries.map((e) =>
          e.kind === "run" && e.id === rid
            ? {
                ...e,
                running: false,
                summary: stopped ? null : "The settings page has a ThemeToggle now. It follows the system theme until a choice is made, and keeps the choice in localStorage.",
                stat: change?.stat ?? null,
                verified: stopped ? null : true,
                verifyCommand: stopped ? null : "npm test",
                gaveUp: stopped ? "stopped" : null,
                toolCalls: e.steps.length,
                seconds: Math.round((Date.now() - e.at) / 1000),
                after: "tree-r1",
              }
            : e,
        ),
      });
    });
  }

  // 2. Finished: an applied request, then a multi-file change whose check failed, not applied yet.
  {
    const id = "s-dates";
    const at = now - 130 * MINUTE;
    const r1 = "r-dates-1";
    const r2 = "r-dates-2";
    runDiffs.set(r1, DIFF_DATES_1);
    runDiffs.set(r2, DIFF_DATES_2);
    changeBefore.set(r1, null);
    changeBefore.set(r2, null);
    sessions.set(id, {
      id,
      title: "Fix the flaky date parsing test",
      repository: `${HOME}\\ledger-api`,
      createdAt: at,
      updatedAt: now - 18 * MINUTE,
      worktree: `C:\\Users\\you\\AppData\\Local\\Temp\\nook-rs\\code\\${id}`,
      baseCommit: "4c3b2a1",
      baseline: "tree-d2",
      verify: "gradle test",
      change: changeOf(DIFF_DATES_2),
      entries: [
        task("The DateParserTest fails about one run in five on CI. Find out why and fix it.", at),
        run({
          id: r1,
          at: at + 1000,
          model: "Qwen3-Coder 30B-A3B",
          steps: [
            "loading Qwen3-Coder 30B-A3B on the GPU",
            "making a scratch copy of ledger-api",
            "listing .",
            "searching for DateParser",
            "reading src/main/java/ledger/DateParser.java",
            "reading src/test/java/ledger/DateParserTest.java",
            "running gradle test --tests DateParserTest",
            "reading build/test-results/test/TEST-ledger.DateParserTest.xml",
            "editing src/main/java/ledger/DateParser.java",
            "editing src/test/java/ledger/DateParserTest.java",
            "running gradle test",
            "verifying the final tree with gradle test",
          ],
          summary:
            "The test built its expected date with the system zone while the parser read \"today\" from the same clock at a different moment, so it failed whenever a CI run crossed midnight. " +
            "The parser now takes a Clock, and the test pins it to 2026-03-14T23:30Z.",
          stat: statOf(DIFF_DATES_1),
          verified: true,
          verifyCommand: "gradle test",
          toolCalls: 11,
          seconds: 187,
          before: "tree-d0",
          after: "tree-d1",
          context: ctx(21400, 1),
        }),
        note(`Applied 2 files to ${HOME}\\ledger-api. Nothing was committed.`, "applied", at + 5 * MINUTE),
        task(
          "Also make the parser accept dates like 14.03.2026 and 4.3.2026, and add tests for the new format and for leap days. Keep ISO dates working as they do now.",
          now - 25 * MINUTE,
        ),
        run({
          id: r2,
          at: now - 25 * MINUTE + 800,
          model: "Qwen3-Coder 30B-A3B",
          steps: [
            "reading src/main/java/ledger/DateParser.java",
            "searching for LegacyDates",
            "reading src/main/java/ledger/LegacyDates.java",
            "writing src/main/java/ledger/DateFormats.java",
            "editing src/main/java/ledger/DateParser.java",
            "editing src/test/java/ledger/DateParserTest.java",
            "running gradle test",
            "reading build/reports/tests/test/index.html",
            "editing src/main/java/ledger/DateParser.java",
            "running gradle test",
            "checking the worker's result with gradle test",
            "verifying the final tree with gradle test",
          ],
          summary:
            "Dates with dots now parse through a list of accepted formats in DateFormats, tried in order; ISO dates are tried first, so they behave as before. " +
            "LegacyDates was only used by the old check and is removed.\n\nOne of the new tests still fails: 29.02.2027 is resolved leniently to the 28th instead of being refused. " +
            "The formats need ResolverStyle.STRICT with \"uuuu\" years.",
          stat: statOf(DIFF_DATES_2),
          verified: false,
          verifyCommand: "gradle test",
          verifyNote: "1 of 12 tests failed",
          verifyOutput: VERIFY_OUTPUT,
          toolCalls: 19,
          seconds: 402,
          before: "tree-d2",
          after: "tree-d3",
          context: ctx(29100, 3),
        }),
      ],
    });
  }

  // 3. Applied, undone and discarded.
  {
    const id = "s-config";
    const at = now - 26 * 60 * MINUTE;
    const r1 = "r-config-1";
    const r2 = "r-config-2";
    const r3 = "r-config-3";
    runDiffs.set(r1, DIFF_CONFIG);
    runDiffs.set(r3, DIFF_SPLIT);
    sessions.set(id, {
      id,
      title: "Rename the config loader and update its callers",
      origin: "editor",
      repository: `${HOME}\\nook-cli`,
      createdAt: at,
      updatedAt: at + 50 * MINUTE,
      worktree: `C:\\Users\\you\\AppData\\Local\\Temp\\nook-rs\\code\\${id}`,
      baseCommit: "0a0b0c0",
      baseline: "tree-c1",
      verify: null,
      change: null,
      entries: [
        task("Rename ConfigLoader to Settings and update its callers.", at),
        run({
          id: r1,
          at: at + 700,
          model: "gpt-oss 20B",
          steps: ["making a scratch copy of nook-cli", "searching for ConfigLoader", "reading src/config.rs", "editing src/config.rs", "editing src/main.rs", "running cargo check"],
          summary: "ConfigLoader is Settings now, in src/config.rs and its one caller in src/main.rs. cargo check passes.",
          stat: statOf(DIFF_CONFIG),
          toolCalls: 5,
          seconds: 96,
          before: "tree-c0",
          after: "tree-c1",
          context: ctx(12800, 0, false),
        }),
        note(`Applied 2 files to ${HOME}\\nook-cli. Nothing was committed.`, "applied", at + 4 * MINUTE),
        task("Now split it into a module per format (json, toml).", at + 20 * MINUTE),
        run({
          id: r2,
          at: at + 20 * MINUTE + 500,
          model: "gpt-oss 20B",
          steps: ["reading src/config.rs", "writing src/config/json.rs", "writing src/config/toml.rs", "running cargo check"],
          summary: "The loader is split into src/config/json.rs and src/config/toml.rs behind a Format enum.",
          stat: " 3 files changed, 88 insertions(+), 41 deletions(-)",
          toolCalls: 4,
          seconds: 141,
          before: "tree-c1",
          after: "tree-c2",
          undone: true,
          context: ctx(15200),
        }),
        note("Undid the last request: the scratch copy is back to how it was before it.", "undone", at + 30 * MINUTE),
        task("Try again, but keep the TOML parser in one file.", at + 35 * MINUTE),
        run({
          id: r3,
          at: at + 35 * MINUTE + 600,
          model: "gpt-oss 20B",
          steps: ["listing src", "writing src/config/mod.rs", "running cargo check"],
          summary: "src/config is a module now; the TOML parser stays in one file.",
          stat: statOf(DIFF_SPLIT),
          toolCalls: 3,
          seconds: 58,
          before: "tree-c1",
          after: "tree-c3",
          context: ctx(9100),
        }),
        note("Changes discarded. The next request starts from what was last applied.", "discarded", at + 45 * MINUTE),
        note("The scratch copy was gone; Nook made a new one from what was last applied.", "info", at + 50 * MINUTE),
      ],
    });
  }

  // 4. The worker gave up: its context was full.
  {
    const id = "s-csv";
    const at = now - 3 * 60 * MINUTE;
    const r1 = "r-csv-1";
    runDiffs.set(r1, DIFF_CSV);
    changeBefore.set(r1, null);
    sessions.set(id, {
      id,
      title: "Port the CSV export to streams",
      origin: "editor",
      repository: `${HOME}\\reports`,
      createdAt: at,
      updatedAt: at + 21 * MINUTE,
      worktree: `C:\\Users\\you\\AppData\\Local\\Temp\\nook-rs\\code\\${id}`,
      baseCommit: "7e7e7e7",
      baseline: null,
      verify: null,
      change: changeOf(DIFF_CSV),
      entries: [
        task(
          [
            "Port the CSV export to Node streams so a 2 GB report doesn't have to fit in memory. Update the callers and the tests.",
            "",
            "Details:",
            "- exportCsv(rows, file) builds every line in an array and writes it once; take an AsyncIterable<Row> instead.",
            "- The header comes from the first row, as now.",
            "- Quote fields that hold a comma, a quote or a new line; double the quotes inside.",
            "- Keep the \\r\\n line endings Excel expects when the options say so.",
            "- reports/monthly.ts and reports/yearly.ts call it; the yearly one reads from the database cursor already.",
            "- test/export.test.ts has the cases; add one with 100 000 rows that checks memory stays under 200 MB.",
            "- Don't touch the PDF export.",
          ].join("\n"),
          at,
        ),
        run({
          id: r1,
          at: at + 900,
          model: "gpt-oss 20B",
          steps: Array.from({ length: 38 }, (_, i) =>
            i % 3 === 0 ? `reading src/export/row${i}.ts` : i % 3 === 1 ? `searching for exportCsv` : `reading test/export.test.ts from line ${i * 20}`,
          ),
          summary: "I converted the writer to a Transform stream and started on the row mapper, but ran out of room before the callers and the tests.",
          stat: statOf(DIFF_CSV),
          gaveUp: "its context was full, even with the oldest file reads and command output dropped; ask for a smaller step",
          toolCalls: 38,
          seconds: 1260,
          before: "tree-e0",
          after: "tree-e1",
          context: ctx(32100, 7),
        }),
      ],
    });
  }

  // 5. A plain answer: no tools, nothing changed.
  {
    const id = "s-hello";
    const at = now - 2 * 24 * 60 * MINUTE;
    sessions.set(id, {
      id,
      title: "hi, what can you do?",
      repository: `${HOME}\\nook-site`,
      createdAt: at,
      updatedAt: at + 20_000,
      worktree: null,
      baseCommit: null,
      baseline: null,
      verify: null,
      change: null,
      entries: [
        task("hi, what can you do?", at),
        run({
          id: "r-hello",
          at: at + 300,
          model: "gpt-oss 20B",
          steps: ["loading gpt-oss 20B on the GPU"],
          summary:
            "Hi! I'm the local model in Nook Code. Pick a project folder and tell me what to change: I read and edit a private copy of its files, " +
            "can run the checks the project allows, and show you every change as a diff. Nothing reaches your files until you press Apply.",
          seconds: 14,
        }),
      ],
    });
  }

  // 6. A failed run and a failure note.
  {
    const id = "s-gradle";
    const at = now - 4 * 24 * 60 * MINUTE;
    sessions.set(id, {
      id,
      title: "Upgrade the build to Gradle 9",
      repository: `${HOME}\\ledger-api`,
      createdAt: at,
      updatedAt: at + 3 * MINUTE,
      worktree: `C:\\Users\\you\\AppData\\Local\\Temp\\nook-rs\\code\\${id}`,
      baseCommit: "4c3b2a1",
      baseline: null,
      verify: null,
      change: null,
      entries: [
        task("Upgrade the build to Gradle 9 and fix whatever breaks.", at),
        run({
          id: "r-gradle",
          at: at + 500,
          model: "Qwen3-Coder 30B-A3B",
          steps: ["loading Qwen3-Coder 30B-A3B on the GPU"],
          error: "The engine could not load Qwen3-Coder 30B-A3B: CUDA out of memory (needed 9.1 GB, 5.2 GB free). Close other programs that use the GPU, or pick a smaller worker.",
          seconds: 41,
        }),
        note(
          "The scratch copy was gone and what was last applied is no longer in the repository. Send again to go on from a fresh copy without it, or Discard.",
          "error",
          at + 3 * MINUTE,
        ),
      ],
    });
  }
}

// ------------------------------------------------------------------ voice input

const speechModel: SpeechModel = { id: "whisper-small", name: "Whisper Small", bytes: 487_601_967 };
let speechInstalled = false;
const speechDownload: SpeechDownload = { available: true, downloading: false, progress: null };
let levelTimer: number | undefined;

function stopLevels(): void {
  window.clearInterval(levelTimer);
  levelTimer = undefined;
}

// ------------------------------------------------------------------ commands

function snapshot(): CodeSnapshot {
  const list = [...sessions.values()].sort((a, b) => b.updatedAt - a.updatedAt);
  return {
    sessions: structuredClone(list),
    workerName: workerName(),
    workerHint,
    workers: workers.map((w) => ({ ...w })),
    workerId,
    phases: { ...phases },
  };
}

function repositoryState(folder: string): RepositoryState {
  const f = folder.trim();
  if (/missing|gone/i.test(f)) return { readiness: "MISSING", root: null, reason: `${f} is not there any more.` };
  if (/^[a-z]:\\?$/i.test(f)) return { readiness: "TOO_BIG", root: f, reason: `${f} is a whole drive. Choose the project's own folder.` };
  if (/^c:\\users\\you\\?$/i.test(f)) return { readiness: "TOO_BIG", root: f, reason: `${f} is your home folder. Choose the project's own folder.` };
  if (/huge|downloads/i.test(f)) {
    const name = f.replace(/[\\/]+$/, "").split(/[\\/]/).pop();
    return {
      readiness: "TOO_BIG",
      root: f,
      reason: `${name} holds more than 20000 files or 1024 MB, too much to copy for each session. Choose the project's own folder.`,
    };
  }
  return { readiness: f.toLowerCase().startsWith(HOME.toLowerCase()) ? "REPOSITORY" : "FOLDER", root: f, reason: null };
}

let registered = false;

export function registerCodeMocks(): void {
  if (registered) return;
  registered = true;
  seed();

  mock("code_snapshot", () => snapshot());

  mock("code_start", async ({ folder, text, verify, origin }) => {
    await sleep(250);
    const st = repositoryState(String(folder));
    if (st.readiness === "MISSING" || st.readiness === "TOO_BIG") throw new Error(st.reason ?? "That folder cannot be used.");
    if (/\\windows\b/i.test(String(folder))) throw new Error(`Nook may not work in ${folder}.`);
    const now = Date.now();
    const id = newId();
    const s: CodeSession = {
      id,
      title: titleFor(String(text ?? "")),
      repository: st.root ?? String(folder),
      createdAt: now,
      updatedAt: now,
      worktree: null,
      baseCommit: null,
      baseline: null,
      verify: (verify as string | null) ?? null,
      change: null,
      entries: [],
      origin: origin === "editor" ? "editor" : "chat",
    };
    sessions.set(id, s);
    try {
      send(s, String(text ?? ""), (verify as string | null) ?? null);
    } catch (e) {
      // Nothing ran: no session to keep.
      sessions.delete(id);
      throw e;
    }
    return structuredClone(sessions.get(id)!);
  });

  mock("code_send", ({ id, text, verify }) => {
    send(require(id), String(text ?? ""), (verify as string | null) ?? null);
  });

  mock("code_stop", ({ id }) => {
    stopSim(String(id));
  });

  mock("code_apply", async ({ id }) => {
    const s = require(id);
    if (runningOf(s)) throw new Error("Wait for the worker to finish.");
    if (s.worktree == null) throw new Error("There is nothing to apply yet.");
    if (s.change == null) throw new Error("There is nothing new to apply.");
    await sleep(400);
    const files = (s.change.diff.match(/^diff --git /gm) ?? []).length;
    put({
      ...s,
      baseline: "tree-" + newId(),
      change: null,
      entries: [...s.entries, note(`Applied ${files}${files === 1 ? " file" : " files"} to ${s.repository}. Nothing was committed.`, "applied")],
    });
    emit();
  });

  mock("code_discard", ({ id }) => {
    const s = require(id);
    if (runningOf(s)) throw new Error("Stop the worker first.");
    const from = s.baseline != null ? "what was last applied." : "the repository's last commit.";
    put({ ...s, change: null, entries: [...s.entries, note("Changes discarded. The next request starts from " + from, "discarded")] });
    emit();
  });

  mock("code_undo", ({ id }) => {
    const s = require(id);
    if (runningOf(s)) throw new Error("Stop the worker first.");
    const r = undoableOf(s);
    if (r == null || s.worktree == null) throw new Error("There is no request to undo.");
    put({
      ...s,
      change: changeBefore.get(r.id) ?? null,
      entries: [
        ...s.entries.map((e) => (e.kind === "run" && e.id === r.id ? { ...e, undone: true } : e)),
        note("Undid the last request: the scratch copy is back to how it was before it.", "undone"),
      ],
    });
    emit();
  });

  mock("code_delete", ({ id }) => {
    const sid = String(id);
    const sim = sims.get(sid);
    if (sim) {
      window.clearTimeout(sim.timer);
      sims.delete(sid);
      delete phases[sim.runId];
    }
    if (sessions.delete(sid)) emit();
  });

  mock("code_rename", ({ id, title }) => {
    const s = sessions.get(String(id));
    const t = String(title ?? "").trim();
    if (!s || !t) return;
    // CodeSession.withTitle keeps updatedAt: a rename does not move the session up.
    put({ ...s, title: t }, false);
    emit();
  });

  mock("code_run_diff", ({ sessionId, runId }) => {
    const s = sessions.get(String(sessionId));
    if (!s || s.worktree == null) return null;
    const r = s.entries.find((e) => e.kind === "run" && e.id === runId) as Run | undefined;
    if (!r || r.undone || r.running || r.before == null || r.after == null || r.before === r.after) return null;
    return runDiffs.get(r.id) ?? null;
  });

  mock("code_next_context", ({ id }): NextContext | null => {
    const s = sessions.get(String(id));
    if (!s || workerId == null) return null;
    let recap = 0;
    for (const e of s.entries) {
      if (e.kind === "task") recap += Math.round(e.text.length / 4) + 12;
      if (e.kind === "run" && e.summary) recap += Math.round(e.summary.length / 4) + 8;
    }
    const size = workerId === "qwen3-coder-30b-a3b" ? 24_576 : WINDOW;
    return { tokens: 5200 + recap, recap, window: size };
  });

  mock("code_repository_state", async ({ folder }) => {
    await sleep(120);
    return repositoryState(String(folder));
  });

  mock("code_recent_repositories", () => {
    const out: string[] = [];
    for (const s of [...sessions.values()].sort((a, b) => b.updatedAt - a.updatedAt)) if (!out.includes(s.repository)) out.push(s.repository);
    return out;
  });

  mock("code_set_worker", ({ modelId }) => {
    if (!workers.some((w) => w.id === modelId)) throw new Error("That model is not installed as a worker.");
    workerId = String(modelId);
    emit();
  });

  mock("code_speech_problem", () =>
    speechInstalled ? null : "No speech model is downloaded yet. Open Settings > Models and download Whisper.",
  );
  mock("code_speech_model", () => ({ ...speechModel }));
  mock("code_speech_download", () => ({ ...speechDownload }));
  mock("code_speech_install", () => {
    if (speechDownload.downloading) return;
    speechDownload.downloading = true;
    speechDownload.progress = 0;
    const timer = window.setInterval(() => {
      speechDownload.progress = Math.min(1, (speechDownload.progress ?? 0) + 0.04);
      if (speechDownload.progress >= 1) {
        window.clearInterval(timer);
        speechDownload.downloading = false;
        speechDownload.progress = null;
        speechInstalled = true;
      }
      mockEmit("downloads", {});
    }, 200);
  });

  mock("speech_start", () => {
    if (!speechInstalled) throw new Error("No speech model is downloaded yet. Open Settings > Models and download Whisper.");
    stopLevels();
    const started = performance.now();
    levelTimer = window.setInterval(() => {
      const t = (performance.now() - started) / 1000;
      // Speech-like: syllables over a slow swell, with quiet gaps.
      const swell = 0.5 + 0.5 * Math.sin(t * 1.3);
      const syllable = Math.abs(Math.sin(t * 9.0));
      const gap = Math.sin(t * 0.7) > 0.85 ? 0.1 : 1;
      mockEmit("speech", { level: Math.min(1, 0.02 + 0.4 * swell * syllable * gap + Math.random() * 0.05) });
    }, 45);
  });
  mock("speech_stop_and_transcribe", async () => {
    stopLevels();
    await sleep(1100);
    return "make the header stick to the top when the page scrolls";
  });
  mock("speech_cancel", () => stopLevels());

  // For poking at states by hand in the browser console.
  (window as unknown as Record<string, unknown>).nookCodeMock = {
    sessions,
    phases,
    setWorker: (id: string | null) => {
      workerId = id;
      emit();
    },
    setSpeechInstalled: (v: boolean) => {
      speechInstalled = v;
    },
    emit,
  };
}
