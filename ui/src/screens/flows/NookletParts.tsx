/**
 * What the Nooklets on the flows' queue (Transcribe, Summarize, Read aloud) share: their runs and
 * downloads as a hook, the choice between a document and pasted text, the switch pills, and the
 * card a run shows (its progress; when done, its notes or summary, its transcript or reading, and
 * the files it wrote).
 */
import { useCallback, useEffect, useRef, useState } from "react";
import {
  flowsAgain,
  flowsCancel,
  flowsDelete,
  flowsInstallState,
  flowsOpenFile,
  flowsOpenFolder,
  flowsRevealFile,
  flowsRuns,
  flowSrc,
  onFlows,
  runProgress,
  type Install,
  type Run,
} from "../../api/flows";
import { READS } from "../../api/convert";
import { inTauri, messageOf, on } from "../../api/ipc";
import { LiveDot, QuietAction, TextLink } from "../../components/Activity";
import { Icon } from "../../components/Icon";
import { ProgressBar } from "../../components/Spinner";
import { clock, nookletDoneText, shortTime, splitPath, stageText, transcriptParagraphs } from "./format";
import { MiniMarkdown } from "./MiniMarkdown";
import { player, TrackPlayer, usePlayer } from "./TrackPlayer";

const newestFirst = (a: Run, b: Run) => b.createdAt - a.createdAt || (b.id < a.id ? -1 : 1);

/**
 * The runs of `flow`, newest first, the downloads while they run, and a tick that moves on when
 * what is installed may have changed (so the page reads its plan again).
 */
export function useFlowRuns(flow: string, say: (message: string) => void) {
  const [runs, setRuns] = useState<Run[]>([]);
  const [install, setInstall] = useState<Install | null>(null);
  const [planTick, setPlanTick] = useState(0);
  const sayRef = useRef(say);
  sayRef.current = say;

  useEffect(() => {
    let alive = true;
    flowsRuns().then(
      (r) => alive && setRuns(r.filter((x) => x.flow === flow).sort(newestFirst)),
      (e) => alive && sayRef.current(messageOf(e)),
    );
    flowsInstallState().then((i) => alive && setInstall(i), () => undefined);
    const off = onFlows((e) => {
      if (e.run && e.run.flow === flow) {
        const run = e.run;
        setRuns((all) => [run, ...all.filter((r) => r.id !== run.id)].sort(newestFirst));
      }
      if (e.removed) {
        const gone = e.removed;
        setRuns((all) => all.filter((r) => r.id !== gone));
      }
      if (e.install !== undefined) {
        setInstall(e.install);
        if (e.install == null || e.install.error != null) setPlanTick((t) => t + 1);
      }
    });
    return () => {
      alive = false;
      off();
    };
  }, [flow]);

  // A model or engine that finishes downloading elsewhere changes the plan too.
  useEffect(() => {
    let last = 0;
    let timer: number | undefined;
    const bump = () => {
      const wait = Math.max(0, 1000 - (Date.now() - last));
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        last = Date.now();
        setPlanTick((t) => t + 1);
      }, wait);
    };
    const offDownloads = on("downloads", bump);
    const offRuntime = on("runtime", bump);
    return () => {
      window.clearTimeout(timer);
      offDownloads();
      offRuntime();
    };
  }, []);

  useEffect(() => () => player.stop(), []);

  const added = useCallback((run: Run) => setRuns((all) => [run, ...all.filter((r) => r.id !== run.id)].sort(newestFirst)), []);
  return { runs, install, planTick, added };
}

export function ModePill({ text, icon, selected, onClick }: { text: string; icon?: string; selected: boolean; onClick: () => void }) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={selected}
      className={selected ? "nk-kind-pill nk-kind-pill--selected" : "nk-kind-pill"}
      onClick={onClick}
    >
      {icon && <Icon name={icon} size={14} />}
      <span>{text}</span>
    </button>
  );
}

/** Windows' Open dialog for a document; null when nothing was chosen. */
export async function chooseDocument(title: string): Promise<string | null> {
  if (inTauri) {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({
      title,
      multiple: false,
      directory: false,
      filters: [
        { name: "Documents", extensions: READS.filter((e) => !["png", "jpg", "jpeg", "jfif", "jpe", "webp", "bmp", "dib", "tiff", "tif", "gif", "ico"].includes(e)) },
        { name: "All files", extensions: ["*"] },
      ],
    });
    return typeof picked === "string" ? picked : null;
  }
  const typed = window.prompt(title, "C:\\Users\\you\\Documents\\tenancy-agreement.pdf");
  return typed && typed.trim() ? typed.trim() : null;
}

/** A document dropped or chosen, or text pasted in. */
export function DocumentInput({
  mode,
  file,
  text,
  hover,
  placeholder,
  onMode,
  onChoose,
  onText,
}: {
  mode: "FILE" | "TEXT";
  file: string | null;
  text: string;
  hover: boolean;
  placeholder: string;
  onMode: (mode: "FILE" | "TEXT") => void;
  onChoose: () => void;
  onText: (text: string) => void;
}) {
  return (
    <>
      <div className="fl-form__modes">
        <div className="nk-kind-switch" role="tablist">
          <ModePill text="Document" icon="file-edit" selected={mode === "FILE"} onClick={() => onMode("FILE")} />
          <ModePill text="Paste text" icon="text" selected={mode === "TEXT"} onClick={() => onMode("TEXT")} />
        </div>
      </div>
      {mode === "TEXT" ? (
        <textarea
          className="nl-paste body2"
          value={text}
          placeholder={placeholder}
          onChange={(e) => onText(e.target.value)}
          aria-label="The text"
        />
      ) : (
        <FileZone file={file} hover={hover} onChoose={onChoose} icon="text" empty="Drop a document here" what="PDF, Word, web pages, e-books, text, slides and more" />
      )}
    </>
  );
}

/** The drop zone: empty, a file dragged over it, or the chosen file. */
export function FileZone({
  file,
  hover,
  onChoose,
  icon,
  empty,
  what,
}: {
  file: string | null;
  hover: boolean;
  onChoose: () => void;
  icon: string;
  empty: string;
  what: string;
}) {
  const classes = ["fl-drop", hover ? "fl-drop--hover" : "", file && !hover ? "fl-drop--chosen" : ""].filter(Boolean).join(" ");
  if (file && !hover) {
    const { name, folder } = splitPath(file);
    return (
      <div className={classes} role="button" tabIndex={0} onClick={onChoose} onKeyDown={(e) => e.key === "Enter" && onChoose()}>
        <span className="fl-drop__file-icon">
          <Icon name={icon} size={20} />
        </span>
        <span className="fl-drop__file">
          <span className="subtitle2 nc-ellipsis">{name}</span>
          <span className="caption text-tertiary nc-ellipsis">{folder}</span>
        </span>
        <span className="caption fl-drop__change">Change</span>
      </div>
    );
  }
  return (
    <div className={classes} role="button" tabIndex={0} onClick={onChoose} onKeyDown={(e) => e.key === "Enter" && onChoose()}>
      <Icon name="upload" size={24} color={hover ? "var(--primary-variant)" : "var(--text-secondary)"} />
      <div className="subtitle2">{hover ? "Drop it here" : empty}</div>
      <div className="caption text-tertiary">or click to choose · {what}</div>
    </div>
  );
}

/** How many transcript paragraphs a card shows before "Show all". */
const PREVIEW = 4;

/** A run of Transcribe, Summarize or Read aloud. */
export function NookletRunCard({ run, onError }: { run: Run; onError: (message: string) => void }) {
  const fail = (e: unknown) => onError(messageOf(e));
  const icon =
    run.source === "MICROPHONE" ? "mic" : run.source === "TEXT" ? "text" : run.flow === "transcribe" ? "audio-file" : "file-edit";
  const summaryFile = run.files.find((f) => f.endsWith(".md"));
  const copy = (text: string, what: string) =>
    navigator.clipboard.writeText(text).then(
      () => onError(`${what} copied.`),
      () => onError(`Could not copy the ${what.toLowerCase()}.`),
    );
  return (
    <div className="fl-card fl-run">
      <div className="fl-run__head">
        <span className="fl-run__icon">
          <Icon name={icon} size={16} />
        </span>
        <span className="subtitle2 fl-run__name" title={run.source === "TEXT" ? undefined : run.input}>
          {run.inputName}
        </span>
        <span className="nc-flex-spacer" />
        <span className="caption text-tertiary">{shortTime(run.createdAt)}</span>
      </div>

      {run.status === "QUEUED" && <Meta text="Queued: starts when the run before it is done." />}
      {run.status === "RUNNING" && <Progress run={run} onStop={() => flowsCancel(run.id).catch(fail)} />}
      {run.status === "DONE" && (
        <>
          {run.flow === "read-aloud" && run.audio && (
            <TrackPlayer id={run.id} src={flowSrc(run.audio)} seconds={run.durationSeconds} label={run.voiceName ?? undefined} onError={onError} />
          )}
          {run.note && <Meta text={run.note} warn />}
          <Meta text={nookletDoneText(run)} />
          {run.summary && <MiniMarkdown text={run.summary} />}
          {run.flow === "transcribe" && <Transcript run={run} />}
          {run.flow === "read-aloud" && <Reading run={run} />}
        </>
      )}
      {run.status === "FAILED" && <Meta text={run.error ?? "It did not work."} error />}
      {run.status === "CANCELLED" && <Meta text="Stopped." />}

      <div className="fl-run__actions">
        {run.status === "QUEUED" && <QuietAction text="Cancel" icon="close" onClick={() => flowsCancel(run.id).catch(fail)} />}
        {run.status === "DONE" && (
          <>
            {run.summary && (
              <QuietAction text={run.flow === "transcribe" ? "Copy notes" : "Copy summary"} icon="copy" onClick={() => copy(run.summary ?? "", run.flow === "transcribe" ? "Notes" : "Summary")} />
            )}
            {run.flow === "transcribe" && run.segments.length > 0 && (
              <QuietAction
                text="Copy transcript"
                icon="copy"
                onClick={() => copy(transcriptParagraphs(run).map((p) => p.text).join("\n\n"), "Transcript")}
              />
            )}
            {summaryFile && run.flow === "summarize" && (
              <QuietAction text="Open" icon="launch" onClick={() => flowsOpenFile(run.id, summaryFile).catch(fail)} />
            )}
            {run.flow === "read-aloud" && run.audio && (
              <QuietAction text="Show in folder" icon="folder-open" onClick={() => flowsRevealFile(run.id, run.audio!).catch(fail)} />
            )}
            {run.flow !== "read-aloud" && <QuietAction text="Open folder" icon="folder-open" onClick={() => flowsOpenFolder(run.id).catch(fail)} />}
            <QuietAction text="Run again" icon="redo" onClick={() => flowsAgain(run.id).catch(fail)} />
            <DeleteAction
              onDelete={() => {
                player.stop(run.id);
                flowsDelete(run.id).catch(fail);
              }}
            />
          </>
        )}
        {(run.status === "FAILED" || run.status === "CANCELLED") && (
          <>
            <QuietAction text="Try again" icon="refresh" onClick={() => flowsAgain(run.id).catch(fail)} />
            <QuietAction text="Remove" icon="trash" onClick={() => flowsDelete(run.id).catch(fail)} />
          </>
        )}
      </div>
    </div>
  );
}

function Progress({ run, onStop }: { run: Run; onStop: () => void }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [run.startedAt]);
  return (
    <div className="fl-run__progress">
      <div className="fl-run__stage">
        <LiveDot />
        <span className="body2 text-secondary fl-run__stage-text">{stageText(run)}</span>
        {run.startedAt != null && <span className="numeric text-tertiary">{clock(now - run.startedAt)}</span>}
        <QuietAction text="Stop" icon="stop" onClick={onStop} />
      </div>
      <div className="fl-bar">
        <ProgressBar progress={runProgress(run)} height={6} />
      </div>
    </div>
  );
}

/** What was said, a few paragraphs at first, each with its time. */
function Transcript({ run }: { run: Run }) {
  const [all, setAll] = useState(!run.summary);
  const paragraphs = transcriptParagraphs(run);
  if (paragraphs.length === 0) return null;
  const shown = all ? paragraphs : paragraphs.slice(0, PREVIEW);
  return (
    <div className="fl-preview">
      {run.summary && <div className="overline text-tertiary">Transcript</div>}
      <div className="fl-preview__lines selectable">
        {shown.map((p, i) => (
          <div key={i} className="nl-para">
            <span className="numeric text-tertiary nl-para__at">{clock(p.at * 1000)}</span>
            <span className="body2">{p.text}</span>
          </div>
        ))}
      </div>
      {paragraphs.length > PREVIEW && (
        <div className="fl-preview__links">
          <TextLink text={all ? "Show less" : `Show all ${paragraphs.length} paragraphs`} onClick={() => setAll(!all)} />
        </div>
      )}
    </div>
  );
}

/** The lines read, the one playing lit; a click plays from it. */
function Reading({ run }: { run: Run }) {
  const [open, setOpen] = useState(false);
  const state = usePlayer();
  const mine = state.id === run.id;
  if (run.segments.length === 0) return null;
  const at = mine ? state.time : -1;
  return (
    <div className="fl-preview">
      {open && (
        <div className="fl-preview__lines nl-reading selectable">
          {run.segments.map((s, i) => (
            <button
              key={i}
              type="button"
              className={at >= s.start && at < s.end + 0.2 ? "nl-reading__line nl-reading__line--now body2" : "nl-reading__line body2"}
              title="Play from here"
              onClick={() => {
                if (!run.audio) return;
                const d = run.durationSeconds;
                if (!mine) {
                  player.play(run.id, flowSrc(run.audio), () => undefined);
                  const once = () => {
                    if (player.get().duration > 0) player.seek(run.id, s.start / player.get().duration);
                    else window.setTimeout(once, 50);
                  };
                  window.setTimeout(once, 50);
                } else if (d > 0) player.seek(run.id, s.start / (state.duration || d));
              }}
            >
              {s.text}
            </button>
          ))}
        </div>
      )}
      <div className="fl-preview__links">
        <TextLink text={open ? "Hide the text" : "Read along"} onClick={() => setOpen(!open)} />
      </div>
    </div>
  );
}

/** Deleting a run removes its files, so the first click asks and the second deletes. */
function DeleteAction({ onDelete }: { onDelete: () => void }) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = window.setTimeout(() => setArmed(false), 4000);
    return () => window.clearTimeout(t);
  }, [armed]);
  return <QuietAction text={armed ? "Delete the files?" : "Delete"} icon="trash" onClick={() => (armed ? onDelete() : setArmed(true))} />;
}

export function Meta({ text, error = false, warn = false }: { text: string; error?: boolean; warn?: boolean }) {
  return <div className={error ? "caption text-error" : warn ? "caption text-warning" : "caption text-tertiary"}>{text}</div>;
}
