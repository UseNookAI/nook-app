/** CodeModels.kt: the worker model picker and the composer's voice input. */
import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  codeSetWorker,
  codeSpeechDownload,
  codeSpeechInstall,
  codeSpeechModel,
  codeSpeechProblem,
  onSpeechLevel,
  speechCancel,
  speechStart,
  speechStopAndTranscribe,
  type CodeSnapshot,
  type SpeechDownload,
  type SpeechModel,
} from "../../api/code";
import { messageOf } from "../../api/ipc";
import { TextLink } from "../../components/Activity";
import { Button } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { DropdownMenu } from "../../components/Popover";
import { ProgressBar, Spinner } from "../../components/Spinner";
import { MenuDivider, StyledMenuItem } from "../../components/StyledMenuItem";
import { ToolbarPill } from "../hub/ComposerControls";
import { useCodeActions, useCodeSnapshot, type CodeActions } from "./useCode";
import { PromptIconButton, WaveformVisualizer } from "./WaveformVisualizer";
import "./code.css";

/** The local model that does the work. Under the greeting on the start page, and in a session's header. */
export function ModelBar({ snapshot, actions, onOpenModels }: { snapshot: CodeSnapshot; actions: CodeActions; onOpenModels: () => void }) {
  return <WorkerPicker snapshot={snapshot} onChoose={(id) => actions.run(() => codeSetWorker(id))} onOpenModels={onOpenModels} />;
}

/** [ModelBar] reading the snapshot itself, for a header outside the Code screens (the Code page's Nook panel). */
export function WorkerBar({ onOpenModels }: { onOpenModels: () => void }) {
  const snapshot = useCodeSnapshot();
  const actions = useCodeActions();
  return <ModelBar snapshot={snapshot} actions={actions} onOpenModels={onOpenModels} />;
}

/** The installed worker models, the chosen one ticked, and the way to more. */
export function WorkerPicker({
  snapshot,
  onChoose,
  onOpenModels,
}: {
  snapshot: CodeSnapshot;
  onChoose: (id: string) => void;
  onOpenModels: () => void;
}) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  const close = useCallback(() => setOpen(false), []);
  const workers = snapshot.workers;
  return (
    <>
      <ToolbarPill
        ref={anchor}
        text={snapshot.workerName ?? "Choose a model"}
        icon="cpu"
        onClick={() => (workers.length === 0 ? onOpenModels() : setOpen(!open))}
        expanded={open}
        emphasised={snapshot.workerName == null}
        maxWidth={220}
      />
      <DropdownMenu anchor={anchor} open={open} onClose={close} role="menu">
        {/* The tested workers, then every other installed chat model: the person may pick any. */}
        {workers.map((w, i) => (
          <div key={w.id}>
            {!w.tested && (i === 0 || workers[i - 1].tested) && <MenuDivider />}
            <ChoiceRow
              text={w.name}
              selected={w.id === snapshot.workerId}
              caption={w.tested ? null : "Not tested with Code"}
              captionWarns={false}
              onClick={() => {
                setOpen(false);
                onChoose(w.id);
              }}
            />
          </div>
        ))}
        <MenuDivider />
        <StyledMenuItem
          text="More models…"
          icon="download"
          onClick={() => {
            setOpen(false);
            onOpenModels();
          }}
        />
      </DropdownMenu>
    </>
  );
}

/** A menu row with a tick when chosen and an optional second line. */
function ChoiceRow({
  text,
  selected,
  caption = null,
  captionWarns = true,
  onClick,
}: {
  text: string;
  selected: boolean;
  caption?: string | null;
  captionWarns?: boolean;
  onClick: () => void;
}) {
  return (
    <button type="button" role="menuitemradio" aria-checked={selected} className="nc-choice-row" onClick={onClick}>
      <Icon name="check" size={14} color={selected ? "var(--text-primary)" : "transparent"} />
      <span className="nc-choice-row__text">
        <span className="body2" style={{ fontWeight: selected ? 600 : 400 }}>
          {text}
        </span>
        {caption != null && (
          <span className="caption" style={{ color: captionWarns ? "var(--warning)" : "var(--text-tertiary)" }}>
            {caption}
          </span>
        )}
      </span>
    </button>
  );
}

// ------------------------------------------------------------------ voice input

type Speech = "IDLE" | "RECORDING" | "WRITING";

/**
 * Voice input for the composer: talk, then click again and the words land in the box. With no
 * speech model installed, it offers to install Nook's.
 */
export function SpeechButton({ enabled, onText, onError }: { enabled: boolean; onText: (text: string) => void; onError: (message: string) => void }) {
  const [state, setState] = useState<Speech>("IDLE");
  const stateRef = useRef<Speech>("IDLE");
  const [levels, setLevels] = useState<number[]>([]);
  const [install, setInstall] = useState<SpeechModel | null>(null);
  const set = (s: Speech) => {
    stateRef.current = s;
    setState(s);
  };

  // Levels arrive while recording; the last 60 are drawn.
  useEffect(
    () =>
      onSpeechLevel((level) => {
        if (stateRef.current === "WRITING") return;
        setLevels((l) => (l.length >= 60 ? [...l.slice(l.length - 59), level] : [...l, level]));
      }),
    [],
  );

  // Leaving the screen mid-recording drops the recording; one still starting drops itself when
  // it comes back to no screen.
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      if (stateRef.current === "RECORDING") speechCancel().catch(() => undefined);
    };
  }, []);

  const start = async () => {
    try {
      const problem = await codeSpeechProblem();
      if (problem != null) {
        const model = await codeSpeechModel();
        if (model != null) setInstall(model);
        else onError(problem);
        return;
      }
    } catch (e) {
      onError(messageOf(e));
      return;
    }
    try {
      await speechStart();
      if (!mounted.current) {
        speechCancel().catch(() => undefined);
        return;
      }
      set("RECORDING");
    } catch (e) {
      onError(messageOf(e) || "The microphone could not start.");
    }
  };

  const stop = async () => {
    set("WRITING");
    try {
      const text = await speechStopAndTranscribe();
      if (text.trim()) onText(text);
    } catch (e) {
      onError(messageOf(e) || "Could not turn that into text.");
    } finally {
      set("IDLE");
      setLevels([]);
    }
  };

  return (
    <span className="nc-speech">
      {state === "RECORDING" && <WaveformVisualizer amplitudes={levels} width={120} height={24} barColor="var(--error)" />}
      {state === "IDLE" && (
        <PromptIconButton onClick={start} icon="mic" label="Speak" hoverBackground="var(--hover)" tint="var(--text-secondary)" enabled={enabled} />
      )}
      {state === "RECORDING" && (
        <PromptIconButton onClick={stop} icon="stop" label="Stop and write it down" hoverBackground="var(--error-soft)" tint="var(--error)" />
      )}
      {state === "WRITING" && (
        <span className="nc-speech__writing">
          <Spinner size={16} stroke={2} color="var(--text-secondary)" trackColor="color-mix(in srgb, var(--text-secondary) 20%, transparent)" />
        </span>
      )}
      {install && <SpeechInstall model={install} onDismiss={() => setInstall(null)} />}
    </span>
  );
}

/** "Voice input needs a speech model": install Nook's (with its progress), or not now. */
function SpeechInstall({ model, onDismiss }: { model: SpeechModel; onDismiss: () => void }) {
  const [ready, setReady] = useState(false);
  const [starting, setStarting] = useState(false);
  const [download, setDownload] = useState<SpeechDownload | null>(null);

  // Ready when the model and its engine are both in place; checked while the card is open.
  useEffect(() => {
    let alive = true;
    let timer: number | undefined;
    const check = async () => {
      const [problem, d] = await Promise.all([codeSpeechProblem().catch(() => "?"), codeSpeechDownload().catch(() => null)]);
      if (!alive) return;
      setDownload(d);
      if (problem == null) {
        setReady(true);
        return;
      }
      timer = window.setTimeout(check, d?.downloading ? 500 : 1500);
    };
    check();
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [model.id]);

  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") onDismiss();
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [onDismiss]);

  const downloading = download?.downloading === true;
  const progress = download?.progress ?? 0;
  const size = model.bytes > 0 ? ` (${Math.floor(model.bytes / 1_000_000)} MB)` : "";

  return createPortal(
    <div
      className="nk-scrim"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onDismiss();
      }}
    >
      <div className="nc-speech-card" role="dialog">
        <div className="h6">{ready ? "Voice input is ready" : "Voice input needs a speech model"}</div>
        <div className="body2 text-secondary">
          {ready
            ? "Click the speech icon, talk, and click it again: the words go into the box."
            : `Nook turns what you say into text with ${model.name}${size}. It runs on this computer; nothing you say leaves it.`}
        </div>
        {ready ? (
          <Button text="Done" onClick={onDismiss} className="nc-wide-button" />
        ) : downloading || starting ? (
          <>
            <ProgressBar progress={progress} color="var(--primary)" />
            <div className="caption text-tertiary">
              Downloading {Math.floor(progress * 100)}%. You can close this; it keeps going.
            </div>
          </>
        ) : (
          <div className="nc-speech-card__choices">
            <Button
              text={`Install ${model.name}`}
              disabled={!download?.available}
              className="nc-wide-button"
              onClick={() => {
                if (download?.available) {
                  setStarting(true);
                  codeSpeechInstall().catch(() => setStarting(false));
                }
              }}
            />
            <TextLink text="Not now" onClick={onDismiss} />
          </div>
        )}
      </div>
    </div>,
    document.body,
  );
}
