/** One translation on the Flows page (TranslateAudioFlow.kt RunCard, AudioRow, TranslationPreview). */
import { useEffect, useState } from "react";
import { flowSrc, runProgress, type Language, type Run } from "../../api/flows";
import { Chip, LiveDot, QuietAction, TextLink } from "../../components/Activity";
import { Icon } from "../../components/Icon";
import { ProgressBar } from "../../components/Spinner";
import { clock, doneText, languageName, shortTime, stageText, voiceText } from "./format";
import { TrackPlayer } from "./TrackPlayer";

/** How many translated lines a finished run shows before "Show all". */
const PREVIEW_LINES = 6;

export interface RunActions {
  onCancel: () => void;
  onDelete: () => void;
  onAgain: () => void;
  onOpenFolder: () => void;
  onOpenVideo: () => void;
  onCopy: () => void;
}

export function RunCard({
  run,
  languages,
  actions,
  onError,
}: {
  run: Run;
  languages: Language[];
  actions: RunActions;
  onError: (message: string) => void;
}) {
  const from = run.detectedLanguage ?? run.sourceLanguage;
  const to = languageName(run.targetLanguage, languages);
  return (
    <div className="fl-card fl-run">
      <div className="fl-run__head">
        <span className="fl-run__icon">
          <Icon name={run.source === "MICROPHONE" ? "mic" : run.video ? "video" : "audio-file"} size={16} />
        </span>
        <span className="subtitle2 fl-run__name" title={run.input}>
          {run.inputName}
        </span>
        <Chip text={`${from ? languageName(from, languages) : "…"} → ${to}`} />
        <span className="nc-flex-spacer" />
        <span className="caption text-tertiary">{shortTime(run.createdAt)}</span>
      </div>

      {run.status === "QUEUED" && <Meta text="Queued: starts when the translation before it is done." />}
      {run.status === "RUNNING" && <Progress run={run} onStop={actions.onCancel} />}
      {run.status === "DONE" && (
        <>
          {run.audio && (
            <TrackPlayer
              id={run.id}
              src={flowSrc(run.audio)}
              // A file's track is laid on its timeline, so it is as long as the file.
              seconds={run.source === "FILE" ? run.durationSeconds : 0}
              label={voiceText(run) ?? to}
              onError={onError}
            />
          )}
          {run.note && <Meta text={run.note} warn={run.audio == null} />}
          <Meta text={doneText(run)} />
          <Preview run={run} />
        </>
      )}
      {run.status === "FAILED" && <Meta text={run.error ?? "The translation failed."} error />}
      {run.status === "CANCELLED" && <Meta text="Stopped." />}

      {/* Pulled left by the actions' own inset, so their text lines up with the title's. */}
      <div className="fl-run__actions">
        {run.status === "QUEUED" && <QuietAction text="Cancel" icon="close" onClick={actions.onCancel} />}
        {run.status === "DONE" && (
          <>
            {run.video && <QuietAction text="Open video" icon="video" onClick={actions.onOpenVideo} />}
            <QuietAction text="Open folder" icon="folder-open" onClick={actions.onOpenFolder} />
            {run.segments.length > 0 && <QuietAction text="Copy translation" icon="copy" onClick={actions.onCopy} />}
            <QuietAction text="Run again" icon="redo" onClick={actions.onAgain} />
            <DeleteAction onDelete={actions.onDelete} />
          </>
        )}
        {(run.status === "FAILED" || run.status === "CANCELLED") && (
          <>
            <QuietAction text="Try again" icon="refresh" onClick={actions.onAgain} />
            <QuietAction text="Remove" icon="trash" onClick={actions.onDelete} />
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
      {/* What has been heard so far, while it is translated and spoken. */}
      {run.segments.length > 0 && (
        <div className="body2 text-tertiary fl-run__glimpse selectable">
          {(run.segments[0].translation ?? run.segments[0].text).trim()}
          {run.segments.length > 1 ? " …" : ""}
        </div>
      )}
    </div>
  );
}

/** The translation, a few lines at first, with the original under each on request. */
function Preview({ run }: { run: Run }) {
  const [all, setAll] = useState(false);
  const [original, setOriginal] = useState(false);
  if (run.segments.length === 0) return null;
  const shown = all ? run.segments : run.segments.slice(0, PREVIEW_LINES);
  return (
    <div className="fl-preview">
      <div className="fl-preview__lines selectable">
        {shown.map((s, i) => (
          <div key={i} className="fl-preview__line">
            <div className="body2">{s.translation ?? ""}</div>
            {original && <div className="caption text-tertiary">{s.text}</div>}
          </div>
        ))}
      </div>
      <div className="fl-preview__links">
        {run.segments.length > PREVIEW_LINES && (
          <TextLink text={all ? "Show less" : `Show all ${run.segments.length} lines`} onClick={() => setAll(!all)} />
        )}
        <TextLink text={original ? "Hide the original" : "Show the original"} onClick={() => setOriginal(!original)} />
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

function Meta({ text, error = false, warn = false }: { text: string; error?: boolean; warn?: boolean }) {
  return <div className={error ? "caption text-error" : warn ? "caption text-warning" : "caption text-tertiary"}>{text}</div>;
}
