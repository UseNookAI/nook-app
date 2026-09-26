/**
 * CodeSessionScreen.kt: the start page (no session open) and a session's page, with the thread
 * and composer they share with the Code page's Nook panel (SessionColumn).
 */
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  THINKING,
  codeApply,
  codeDiscard,
  codeNextContext,
  codeRecentRepositories,
  codeRepositoryState,
  codeRunDiff,
  codeSend,
  codeStart,
  codeStop,
  codeUndo,
  type CodeSession,
  type CodeSnapshot,
  type EditorContext,
  type NextContext,
  type Note,
  type Run,
  type Task,
} from "../../api/code";
import { messageOf } from "../../api/ipc";
import nookLogo from "../../assets/images/nook-primary-logo.svg";
import { LiveDot, QuietAction } from "../../components/Activity";
import { repoName } from "../../components/paths";
import { Greeting } from "../hub/Greeting";
import { KindSwitch, type ChatKind } from "../hub/KindSwitch";
import { PagePane } from "../hub/PagePane";
import { CodeComposer } from "./CodeComposer";
import { FoldToggle } from "./CodeControls";
import { ModelBar } from "./CodeModels";
import { liveReading, nextReading, peakLine } from "./ContextMeter";
import { parseDiff, statLine } from "./Diffs";
import { openFolder } from "./FolderPicker";
import { CodeBlock, ReplyToolbar, SelectableText, copyText } from "./ReplyParts";
import { capitalise, duration, isRunning, lastRun, time, undoable, type CodeActions } from "./useCode";
import "./code.css";

// ====================================================================== start page

/** No session open: say what Code mode does, and take the first request. */
export function CodeStartScreen({
  snapshot,
  actions,
  onStarted,
  onOpenModels,
  kind = "CHAT",
  onKind = () => undefined,
}: {
  snapshot: CodeSnapshot;
  actions: CodeActions;
  onStarted: (id: string) => void;
  onOpenModels: () => void;
  kind?: ChatKind;
  onKind?: (k: ChatKind) => void;
}) {
  const [text, setText] = useState("");
  const repositories = useRecentRepositories(snapshot.sessions.length);
  const [repository, setRepository] = useState<string | null>(null);
  const repo = repository ?? repositories[0] ?? null;
  const [starting, setStarting] = useState(false);
  // Any folder works (a repository, or a plain folder through a private copy); one that is gone
  // or too big to copy says so before Send.
  const refused = useRepositoryRefusal(repo);

  return (
    <PagePane>
      {/* The same page as a new chat: greeting and composer together in the middle. */}
      <div className="nc-start">
        <div className="nc-start__column">
          <Greeting subtitle="What should Nook build?" />
          {/* A chat or a video; and for a chat, the worker model. */}
          <div className="nc-start__controls">
            <KindSwitch kind={kind} onKind={onKind} />
            <ModelBar snapshot={snapshot} actions={actions} onOpenModels={onOpenModels} />
          </div>
          <CodeComposer
            text={text}
            onTextChange={setText}
            placeholder={repo == null ? "Choose a folder, then describe the change" : `Describe the change in ${repoName(repo)}`}
            repository={repo}
            repoLocked={false}
            repositories={repositories}
            onRepository={setRepository}
            workerName={snapshot.workerName}
            onNotice={actions.say}
            running={starting}
            repositoryReady={refused == null}
            onSend={() => {
              if (repo == null) return;
              const t = text;
              setStarting(true);
              actions.run(async () => {
                try {
                  const s = await codeStart(repo, t, null);
                  setText("");
                  onStarted(s.id);
                } finally {
                  setStarting(false);
                }
              });
            }}
            onStop={() => undefined}
          />
          {repo != null && refused != null && <div className="caption text-warning nc-start__note">{refused}</div>}
          {snapshot.workerName == null && (
            <div className="caption text-warning nc-start__note">
              Code mode needs a worker model. Download {snapshot.workerHint} in Settings &gt; Models.
            </div>
          )}
        </div>
      </div>
    </PagePane>
  );
}

/** Folders of earlier sessions, newest first; git or not, Code mode works on them. Read again when [key] changes. */
function useRecentRepositories(key: number): string[] {
  const [list, setList] = useState<string[]>([]);
  useEffect(() => {
    let alive = true;
    codeRecentRepositories()
      .then((l) => alive && setList(l))
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [key]);
  return list;
}

/** Why Code mode cannot work on [folder] (gone, too big to copy, not allowed), or null when it can. */
export function useRepositoryRefusal(folder: string | null): string | null {
  const [refused, setRefused] = useState<string | null>(null);
  useEffect(() => {
    setRefused(null);
    if (folder == null) return;
    let alive = true;
    codeRepositoryState(folder)
      .then((st) => alive && setRefused(st.readiness === "REPOSITORY" || st.readiness === "FOLDER" ? null : st.reason))
      .catch((e) => alive && setRefused(messageOf(e)));
    return () => {
      alive = false;
    };
  }, [folder]);
  return refused;
}

// ====================================================================== a session

export function CodeSessionScreen({
  session,
  snapshot,
  actions,
  onOpenModels,
}: {
  session: CodeSession;
  snapshot: CodeSnapshot;
  actions: CodeActions;
  onOpenModels: () => void;
}) {
  return (
    <PagePane>
      <div className="nc-session">
        <div className="nc-session__header">
          <span className="subtitle2 nc-session__title" title={session.title}>
            {session.title}
          </span>
          <QuietAction
            text={repoName(session.repository)}
            icon="folder-open"
            title={session.repository}
            onClick={() => actions.run(() => openFolder(session.repository))}
          />
          <span className="nc-flex-spacer" />
          <ModelBar snapshot={snapshot} actions={actions} onOpenModels={onOpenModels} />
        </div>
        <SessionThread session={session} snapshot={snapshot} actions={actions} />
        <div className="nc-session__composer">
          <SessionComposer session={session} snapshot={snapshot} actions={actions} />
        </div>
      </div>
    </PagePane>
  );
}

/**
 * The conversation of a session: each request, the worker's reply with its code, and Nook's
 * notes. It follows the thread: a session opens at its latest entry; later, a new entry or a
 * changed last one (a run finishing) scrolls down, unless the person scrolled up to read.
 * [compact] lays it out for a narrow column (the Code page's Nook panel) rather than the page.
 */
export function SessionThread({
  session,
  snapshot,
  actions,
  compact = false,
}: {
  session: CodeSession;
  snapshot: CodeSnapshot;
  actions: CodeActions;
  compact?: boolean;
}) {
  const id = session.id;
  const entries = session.entries;
  const last = lastRun(session);
  const change = session.change;
  const running = isRunning(session);
  // The reply whose change is not in the person's files yet: the last run still standing.
  let pendingRun: string | null = null;
  if (change != null && !running) {
    for (let i = entries.length - 1; i >= 0; i--) {
      const e = entries[i];
      if (e.kind === "run" && !e.undone && e.error == null) {
        pendingRun = e.id;
        break;
      }
    }
  }
  const canUndo = undoable(session)?.id ?? null;

  const scroller = useRef<HTMLDivElement>(null);
  const seen = useRef({ id: "", count: -1, key: "" });
  // Whether the thread is following its end: until the person scrolls up to read.
  const stick = useRef(true);
  // Scrolls the thread makes itself say nothing about where the person wants to be.
  const autoUntil = useRef(0);
  const autoTop = useRef<number | null>(null);
  const follow = useCallback((smooth: boolean) => {
    const el = scroller.current;
    if (!el) return;
    if (smooth) {
      autoUntil.current = Date.now() + 700;
      el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    } else {
      el.scrollTop = el.scrollHeight;
      autoTop.current = el.scrollTop;
    }
  }, []);
  const lastSteps = last?.steps.length ?? 0;
  const lastKey = entries.length > 0 ? JSON.stringify(entries[entries.length - 1]) : "";

  useLayoutEffect(() => {
    if (entries.length === 0) return;
    if (seen.current.id !== id) seen.current = { id, count: -1, key: "" };
    // Once per change, however often the effect runs.
    const key = `${entries.length}|${lastSteps}|${lastKey}|${change?.diff.length ?? -1}`;
    if (seen.current.key === key) return;
    seen.current.key = key;
    const first = seen.current.count < 0;
    const grew = entries.length > seen.current.count;
    seen.current.count = entries.length;
    if (first) {
      stick.current = true;
      follow(false);
    } else if (grew || stick.current) {
      stick.current = true;
      follow(true);
    }
  }, [id, entries.length, lastSteps, lastKey, change, follow]);

  // A reply's code arrives after it finishes: a thread that is following keeps following.
  const onGrew = useCallback(() => {
    if (stick.current) follow(Date.now() < autoUntil.current);
  }, [follow]);

  return (
    <div
      ref={scroller}
      className={compact ? "nc-thread nc-thread--compact" : "nc-thread"}
      onScroll={(e) => {
        const el = e.currentTarget;
        if (Date.now() < autoUntil.current) return;
        if (autoTop.current != null && Math.abs(el.scrollTop - autoTop.current) < 2) return;
        autoTop.current = null;
        stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
      }}
    >
      <div className="nc-thread__content">
        {entries.map((e) => (
          <div key={e.id} className={compact ? "nc-entry" : "nc-entry nc-entry--page"}>
            {e.kind === "task" && <TaskBubble task={e} />}
            {e.kind === "run" && (
              <RunBlock
                sessionId={id}
                run={e}
                phase={snapshot.phases[e.id] ?? null}
                onStop={() => actions.run(() => codeStop(id))}
                changeDiff={change?.diff ?? null}
                pending={e.id === pendingRun}
                canUndo={e.id === canUndo}
                onApply={() => actions.run(() => codeApply(id))}
                onDiscard={() => actions.run(() => codeDiscard(id))}
                onUndo={() => actions.run(() => codeUndo(id))}
                onGrew={onGrew}
              />
            )}
            {e.kind === "note" && <NoteLine note={e} />}
          </div>
        ))}
        <div className="nc-thread__end" />
      </div>
    </div>
  );
}

/**
 * The composer of an open session: the next request, voice input, the context meter, and send
 * or stop. [editorContext] goes with what is sent (the Code page tells the worker which file is
 * open and what is selected).
 */
export function SessionComposer({
  session,
  snapshot,
  actions,
  compact = false,
  editorContext = null,
}: {
  session: CodeSession;
  snapshot: CodeSnapshot;
  actions: CodeActions;
  compact?: boolean;
  editorContext?: EditorContext | null;
}) {
  const id = session.id;
  const [draft, setDraft] = useState({ id, text: "" });
  const text = draft.id === id ? draft.text : "";
  const setText = (t: string) => setDraft({ id, text: t });
  const entries = session.entries;
  const last = lastRun(session);
  const change = session.change;
  const running = isRunning(session);
  // The context meter: while a run works, how full the engine says its context is; between
  // requests (and before a run's first reply), what the next request starts with.
  const [next, setNext] = useState<NextContext | null>(null);
  useEffect(() => {
    let alive = true;
    codeNextContext(id)
      .then((n) => alive && setNext(n))
      .catch(() => alive && setNext(null));
    return () => {
      alive = false;
    };
  }, [id, entries.length, running, snapshot.workerId]);
  const live = last?.running ? last.context : null;
  const contextReading = live != null ? liveReading(live) : next != null ? nextReading(next) : null;

  return (
    <CodeComposer
      text={text}
      onTextChange={setText}
      placeholder={change != null ? "Ask for more changes, or for fixes" : "Describe the change"}
      repository={session.repository}
      repoLocked
      repositories={[]}
      onRepository={() => undefined}
      workerName={snapshot.workerName}
      onNotice={actions.say}
      running={running}
      onSend={() => {
        const t = text;
        setText("");
        actions.run(() => codeSend(id, t, null, editorContext));
      }}
      onStop={() => actions.run(() => codeStop(id))}
      context={contextReading}
      showRepository={!compact}
    />
  );
}

// ====================================================================== entries

function TaskBubble({ task }: { task: Task }) {
  const long = task.text.split("\n").length > 8 || task.text.length > 700;
  const [all, setAll] = useState(false);
  return (
    <div className="nc-task">
      <div className="nc-task__column">
        <div className="caption text-tertiary nc-task__meta">You · {time(task.at)}</div>
        <div className="nc-task__bubble">
          <SelectableText className={long && !all ? "body1 nc-task__text nc-task__text--clamped" : "body1 nc-task__text"}>{task.text}</SelectableText>
        </div>
        {long && <FoldToggle text={all ? "Show less" : "Show all"} open={all} onClick={() => setAll(!all)} />}
      </div>
    </div>
  );
}

function NookMark() {
  return <img src={nookLogo} alt="Nook" className="nc-nook-mark" draggable={false} />;
}

function RunBlock({
  sessionId,
  run,
  phase,
  onStop,
  changeDiff,
  pending,
  canUndo,
  onApply,
  onDiscard,
  onUndo,
  onGrew,
}: {
  sessionId: string;
  run: Run;
  phase: string | null;
  onStop: () => void;
  changeDiff: string | null;
  pending: boolean;
  canUndo: boolean;
  onApply: () => void;
  onDiscard: () => void;
  onUndo: () => void;
  /** After the reply's code has come in and made it taller. */
  onGrew: () => void;
}) {
  // The code this reply wrote, read once it is done (and again if it is undone or changes).
  const [diff, setDiff] = useState<string | null>(null);
  const changeRef = useRef(changeDiff);
  changeRef.current = changeDiff;
  useEffect(() => {
    if (run.running || run.undone) {
      setDiff(null);
      return;
    }
    let alive = true;
    codeRunDiff(sessionId, run.id)
      .catch(() => null)
      .then((d) => {
        if (!alive) return;
        // For the reply with the pending change that ran before runs kept their end tree, the
        // pending change itself.
        setDiff(d ?? (pending && run.after == null ? changeRef.current : null));
      });
    return () => {
      alive = false;
    };
  }, [sessionId, run.id, run.running, run.undone, run.after, pending]);
  const files = useMemo(() => (diff ? parseDiff(diff) : []), [diff]);
  useLayoutEffect(() => {
    if (files.length > 0) onGrew();
  }, [files, onGrew]);

  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!run.running) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [run.running]);

  const [showSteps, setShowSteps] = useState(false);
  const [showOutput, setShowOutput] = useState(false);

  const liveLine =
    (phase === THINKING ? "Thinking" : phase == null ? (run.steps.length > 0 ? capitalise(run.steps[run.steps.length - 1]) : "Starting") : capitalise(phase)) + "…";

  return (
    <div className="nc-run">
      <NookMark />
      <div className="nc-run__body">
        <div className="nc-run__meta">
          <span className="caption text-tertiary">{[run.model, time(run.at)].filter((x) => x != null).join(" · ")}</span>
          {run.undone && <span className="caption nc-undone-pill">Undone</span>}
        </div>
        {run.running ? (
          <div className="nc-run__live">
            <LiveDot thinking={phase === THINKING} />
            <span className="body1 nc-run__live-text">{liveLine}</span>
            <span className="caption text-tertiary nc-nowrap">{duration((now - run.at) / 1000)}</span>
            <QuietAction text="Stop" icon="stop" onClick={onStop} />
          </div>
        ) : (
          <>
            {run.error != null && run.error.trim() !== "" && <div className="body1 nc-run__error selectable">{run.error}</div>}
            {run.summary != null && run.summary.trim() !== "" && <SelectableText className="body1">{run.summary}</SelectableText>}
            {files.map((f) => (
              <CodeBlock key={f.path} file={f} />
            ))}
            {run.gaveUp != null && run.error == null && <div className="caption text-warning">The worker stopped: {run.gaveUp}.</div>}
          </>
        )}
        {/* A plain answer (no tools, nothing changed) has no steps worth opening once it is done. */}
        {run.steps.length > 0 && (run.running || run.toolCalls > 0 || run.stat != null) && (
          <>
            <FoldToggle
              text={
                (showSteps ? "Hide " : "Show ") +
                `${run.steps.length} steps` +
                (!run.running ? ` · ${run.toolCalls} tool calls · ${duration(run.seconds)}` : "") +
                (!run.running && run.context ? " · " + peakLine(run.context) : "")
              }
              open={showSteps}
              onClick={() => setShowSteps(!showSteps)}
            />
            {showSteps && <StepList steps={run.steps} />}
          </>
        )}
        {!run.running && run.error == null && (
          <>
            {run.verified != null && <VerifyBadge run={run} />}
            <ReplyToolbar
              onCopy={() => {
                const codeText = files.map((f) => f.path + "\n" + f.lines.join("\n")).join("\n\n");
                copyText([run.summary, codeText.trim() ? codeText : null].filter((x) => x != null).join("\n\n"));
              }}
              stat={run.undone ? "Undone" : run.stat != null ? statLine(run.stat, files) : null}
              pending={pending}
              canUndo={canUndo}
              onUndo={onUndo}
              onDiscard={onDiscard}
              onApply={onApply}
            />
            {run.verified === false && run.verifyOutput != null && run.verifyOutput.trim() !== "" && (
              <>
                <FoldToggle
                  text={showOutput ? "Hide the check's output" : "Show the check's output"}
                  open={showOutput}
                  onClick={() => setShowOutput(!showOutput)}
                />
                {showOutput && <pre className="nc-verify-output selectable">{run.verifyOutput}</pre>}
              </>
            )}
          </>
        )}
      </div>
    </div>
  );
}

function StepList({ steps }: { steps: string[] }) {
  return (
    <div className="nc-steps">
      {steps.map((s, i) => (
        <div key={i} className="nc-steps__step">
          {capitalise(s)}
        </div>
      ))}
    </div>
  );
}

/** Whether the reply's check passed: "✓ npm test passed" or "✗ npm test did not pass". */
function VerifyBadge({ run }: { run: Run }) {
  const passed = run.verified === true;
  const label = passed ? `✓ ${run.verifyCommand ?? "check"} passed` : `✗ ${run.verifyCommand ?? "check"} did not pass`;
  return (
    <span className={passed ? "caption nc-verify nc-verify--passed" : "caption nc-verify nc-verify--failed"} title={label}>
      {label}
    </span>
  );
}

function NoteLine({ note }: { note: Note }) {
  const tone = note.tone === "error" ? "nc-note--error" : note.tone === "ok" || note.tone === "applied" ? "nc-note--ok" : "";
  return <div className={`caption nc-note selectable ${tone}`}>{note.text}</div>;
}
