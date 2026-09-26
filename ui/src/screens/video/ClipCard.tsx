/** One clip on the Video page (VideoScreen.kt ClipCard, DeleteAction, RenderProgress). */
import { useEffect, useState, type ReactNode } from "react";
import { clipProgress, type Clip } from "../../api/video";
import { LiveDot, QuietAction } from "../../components/Activity";
import { ProgressBar } from "../../components/Spinner";
import { AviPlayer } from "./AviPlayer";
import { clock, doneText, stageText } from "./format";

/** A raised card on the page, as the composer is. */
export function Card({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={className ? `vd-card ${className}` : "vd-card"}>{children}</div>;
}

export interface ClipActions {
  onCancel: () => void;
  onDelete: () => void;
  onRetry: () => void;
  onReuse: () => void;
  onOpen: () => void;
  onReveal: () => void;
}

export function ClipCard({ clip, autoPlay, actions }: { clip: Clip; autoPlay: boolean; actions: ClipActions }) {
  const aspect = clip.width > 0 && clip.height > 0 ? clip.width / clip.height : 832 / 480;
  return (
    <Card>
      {clip.status === "DONE" && clip.file && <AviPlayer key={clip.file} file={clip.file} autoPlay={autoPlay} aspect={aspect} />}
      {clip.status === "RUNNING" && (
        <div className="vd-clip__running" style={{ aspectRatio: String(aspect) }}>
          <RenderProgress clip={clip} />
        </div>
      )}
      <div className="vd-clip__body">
        <div className="vd-clip__prompt body2">{clip.prompt}</div>
        {clip.status === "QUEUED" && <Meta text="Queued: starts when the clip before it is done." />}
        {clip.status === "DONE" && <Meta text={doneText(clip)} />}
        {clip.status === "FAILED" && <Meta text={clip.error ?? "The clip failed."} error />}
        {clip.status === "CANCELLED" && <Meta text="Stopped." />}
        {/* Pulled left by the actions' own inset, so their text lines up with the prompt's. */}
        <div className="vd-clip__actions">
          {clip.status === "QUEUED" && <QuietAction text="Cancel" icon="close" onClick={actions.onCancel} />}
          {clip.status === "RUNNING" && <QuietAction text="Stop" icon="stop" onClick={actions.onCancel} />}
          {clip.status === "DONE" && (
            <>
              <QuietAction text="Open" icon="launch" onClick={actions.onOpen} />
              <QuietAction text="Show in folder" icon="folder-open" onClick={actions.onReveal} />
              <QuietAction text="Reuse prompt" icon="redo" onClick={actions.onReuse} />
              <DeleteAction onDelete={actions.onDelete} />
            </>
          )}
          {(clip.status === "FAILED" || clip.status === "CANCELLED") && (
            <>
              <QuietAction text="Try again" icon="refresh" onClick={actions.onRetry} />
              <QuietAction text="Remove" icon="trash" onClick={actions.onDelete} />
            </>
          )}
        </div>
      </div>
    </Card>
  );
}

/**
 * Deleting a clip removes its file, so the first click asks and the second deletes. (The card is
 * keyed by the clip, so the armed state belongs to one clip, as `remember(id)` did.)
 */
function DeleteAction({ onDelete }: { onDelete: () => void }) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = window.setTimeout(() => setArmed(false), 4000);
    return () => window.clearTimeout(t);
  }, [armed]);
  return (
    <QuietAction
      text={armed ? "Delete the file?" : "Delete"}
      icon="trash"
      onClick={() => (armed ? onDelete() : setArmed(true))}
    />
  );
}

function RenderProgress({ clip }: { clip: Clip }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [clip.startedAt]);
  return (
    <div className="vd-render">
      <div className="vd-render__stage">
        <LiveDot />
        <span className="body2 text-secondary">{stageText(clip)}</span>
      </div>
      <div className="vd-track">
        <ProgressBar progress={clipProgress(clip)} height={8} />
      </div>
      {clip.startedAt != null && <div className="caption text-tertiary">{clock(now - clip.startedAt)}</div>}
    </div>
  );
}

function Meta({ text, error = false }: { text: string; error?: boolean }) {
  return <div className={error ? "caption text-error" : "caption text-tertiary"}>{text}</div>;
}
