/**
 * TranslateAudioFlow.kt: speech in, the same speech in another language out, with the text and
 * subtitles beside it. The form takes what to translate (the microphone, or a file dropped or
 * chosen), the languages and whether to keep the speaker's voice; anything still to download is
 * one button with its size. The runs follow, newest first, each with its progress while it works
 * and its track when done. New in this port: the microphone. Press, speak, press again, and the
 * translation plays by itself when it is ready.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { onSpeechLevel } from "../../api/code";
import {
  flowsAgain,
  flowsCancel,
  flowsCancelInstall,
  flowsClearInstallError,
  flowsDelete,
  flowsInstall,
  flowsInstallState,
  flowsLanguages,
  flowsOpen,
  flowsOpenFolder,
  flowsPlan,
  flowsRecordCancel,
  flowsRecordStart,
  flowsRecordStop,
  flowsRuns,
  flowsSubmit,
  flowSrc,
  onFlows,
  TRANSLATE_AUDIO,
  type Install,
  type Language,
  type Plan,
  type Run,
} from "../../api/flows";
import { messageOf, on } from "../../api/ipc";
import { QuietAction, TextLink } from "../../components/Activity";
import { Button, IconButton } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { Spinner } from "../../components/Spinner";
import { WaveformVisualizer } from "../code/WaveformVisualizer";
import { clock, defaultTarget, languageName, needsText, splitPath, translationText } from "./format";
import { DownloadLine } from "./DownloadLine";
import { LanguagePicker } from "./LanguagePicker";
import { RunCard } from "./RunCard";
import { player } from "./TrackPlayer";
import { chooseMediaFile, useFileDrop } from "./useFileDrop";

type Mode = "SPEAK" | "FILE";
type Recording = "IDLE" | "STARTING" | "RECORDING" | "SENDING";

/** What the form remembers between visits and restarts (FlowMemory). */
interface Choices {
  mode: Mode;
  file: string | null;
  /** The spoken language's code, or null to detect it. */
  source: string | null;
  /** The language to translate into; null until the first choice (then the computer's own). */
  target: string | null;
  keepVoice: boolean;
}

const STORE = "nook.flows.translate";

function loadChoices(): Choices {
  const fresh: Choices = { mode: "SPEAK", file: null, source: null, target: null, keepVoice: true };
  try {
    const saved = JSON.parse(window.localStorage.getItem(STORE) ?? "null") as Partial<Choices> | null;
    return saved ? { ...fresh, ...saved } : fresh;
  } catch {
    return fresh;
  }
}

function saveChoices(c: Choices) {
  try {
    window.localStorage.setItem(STORE, JSON.stringify(c));
  } catch {
    // Remembering the form is a convenience; without storage it starts afresh next time.
  }
}

const newestFirst = (a: Run, b: Run) => b.createdAt - a.createdAt || (b.id < a.id ? -1 : 1);

export function TranslateFlow({
  say,
  onOpenModels,
  preset = null,
}: {
  say: (message: string) => void;
  onOpenModels: () => void;
  /** A request from the finder ("into German") sets the language to translate into. */
  preset?: { language: string; nonce: number } | null;
}) {
  const [languages, setLanguages] = useState<Language[]>([]);
  const [choices, setChoices] = useState<Choices>(loadChoices);
  const [plan, setPlan] = useState<Plan | null>(null);
  const [install, setInstall] = useState<Install | null>(null);
  const [runs, setRuns] = useState<Run[]>([]);
  const [recording, setRecording] = useState<Recording>("IDLE");
  const [levels, setLevels] = useState<number[]>([]);
  const [recordedAt, setRecordedAt] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());
  /** Bumped whenever what is installed may have changed, so the plan is read again. */
  const [planTick, setPlanTick] = useState(0);
  /** Runs started from the microphone in this visit: they play by themselves when done. */
  const autoplay = useRef(new Set<string>());
  const sayRef = useRef(say);
  sayRef.current = say;
  const recordingRef = useRef(recording);
  recordingRef.current = recording;

  const update = useCallback((patch: Partial<Choices>) => {
    setChoices((c) => {
      const next = { ...c, ...patch };
      saveChoices(next);
      return next;
    });
  }, []);

  // Only a language a voice speaks can be translated into; one remembered from before that no
  // voice speaks gives way to the usual choice.
  const spoken = languages.filter((l) => l.spoken !== false);
  const kept = languages.length === 0 || spoken.some((l) => l.code === choices.target) ? choices.target : null;
  const target = kept ?? (spoken.length > 0 ? defaultTarget(navigator.language, spoken) : "es");
  const { mode, file, source, keepVoice } = choices;
  const sourceSpoken = source != null && spoken.some((l) => l.code === source);

  // The finder's language, once the languages are known: only one a voice speaks.
  const appliedPreset = useRef<number | null>(null);
  useEffect(() => {
    if (!preset || appliedPreset.current === preset.nonce || languages.length === 0) return;
    appliedPreset.current = preset.nonce;
    if (languages.some((l) => l.code === preset.language && l.spoken !== false)) update({ target: preset.language });
    else sayRef.current(`No voice speaks ${languageName(preset.language, languages)} yet, so Nook cannot translate into it.`);
  }, [preset, languages, update]);

  // ---------------------------------------------------------------- reading

  useEffect(() => {
    flowsLanguages().then(setLanguages, (e) => sayRef.current(messageOf(e)));
  }, []);

  useEffect(() => {
    let alive = true;
    flowsRuns().then(
      (r) => alive && setRuns(r.filter((x) => x.flow === TRANSLATE_AUDIO).sort(newestFirst)),
      (e) => alive && sayRef.current(messageOf(e)),
    );
    flowsInstallState().then((i) => alive && setInstall(i), () => undefined);
    const off = onFlows((e) => {
      if (e.run && e.run.flow === TRANSLATE_AUDIO) {
        const run = e.run;
        setRuns((all) => [run, ...all.filter((r) => r.id !== run.id)].sort(newestFirst));
        if (run.status === "DONE" && autoplay.current.has(run.id)) {
          autoplay.current.delete(run.id);
          if (run.audio) player.play(run.id, flowSrc(run.audio), sayRef.current);
        }
        if (run.status === "FAILED" || run.status === "CANCELLED") autoplay.current.delete(run.id);
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
  }, []);

  // A model or engine that finishes downloading elsewhere (Settings › Models) changes the plan too.
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

  useEffect(() => {
    let alive = true;
    flowsPlan(mode === "FILE" ? file : null, mode === "SPEAK", target, keepVoice).then(
      (p) => alive && setPlan(p),
      (e) => alive && sayRef.current(messageOf(e)),
    );
    return () => {
      alive = false;
    };
  }, [mode, file, target, keepVoice, planTick]);

  // ---------------------------------------------------------------- the microphone

  useEffect(
    () =>
      onSpeechLevel((level) => {
        if (recordingRef.current !== "RECORDING") return;
        setLevels((l) => (l.length >= 60 ? [...l.slice(l.length - 59), level] : [...l, level]));
      }),
    [],
  );

  useEffect(() => {
    if (recording !== "RECORDING") return;
    const t = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(t);
  }, [recording]);

  // Leaving the page drops a recording in progress, and stops what plays.
  useEffect(
    () => () => {
      if (recordingRef.current === "RECORDING") flowsRecordCancel().catch(() => undefined);
      player.stop();
    },
    [],
  );

  const ready = plan?.ready === true;

  const startRecording = useCallback(async () => {
    if (recordingRef.current !== "IDLE" || !ready) return;
    player.stop();
    setRecording("STARTING");
    try {
      await flowsRecordStart();
      setLevels([]);
      setRecordedAt(Date.now());
      setNow(Date.now());
      setRecording("RECORDING");
    } catch (e) {
      say(messageOf(e) || "The microphone could not start.");
      setRecording("IDLE");
    }
  }, [ready, say]);

  const stopRecording = useCallback(async () => {
    if (recordingRef.current !== "RECORDING") return;
    setRecording("SENDING");
    try {
      const run = await flowsRecordStop(source, target, keepVoice);
      autoplay.current.add(run.id);
      setRuns((all) => [run, ...all.filter((r) => r.id !== run.id)].sort(newestFirst));
    } catch (e) {
      say(messageOf(e));
    } finally {
      setRecording("IDLE");
      setLevels([]);
    }
  }, [source, target, keepVoice, say]);

  const cancelRecording = useCallback(() => {
    if (recordingRef.current !== "RECORDING") return;
    flowsRecordCancel().catch(() => undefined);
    setRecording("IDLE");
    setLevels([]);
  }, []);

  // Space starts and stops a recording, Escape drops it, unless a field has the keyboard.
  useEffect(() => {
    if (mode !== "SPEAK") return;
    const key = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      if (e.key === " " && !e.repeat) {
        e.preventDefault();
        if (recordingRef.current === "RECORDING") stopRecording();
        else startRecording();
      } else if (e.key === "Escape") cancelRecording();
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [mode, startRecording, stopRecording, cancelRecording]);

  // ---------------------------------------------------------------- files

  const { hover } = useFileDrop(recording === "IDLE", (path) => update({ mode: "FILE", file: path }));
  const choose = () => {
    chooseMediaFile(file ? splitPath(file).folder : null)
      .then((picked) => picked && update({ mode: "FILE", file: picked }))
      .catch(() => undefined);
  };
  const submit = () => {
    if (!file) return;
    flowsSubmit(file, source, target, keepVoice).then(
      (run) => setRuns((all) => [run, ...all.filter((r) => r.id !== run.id)].sort(newestFirst)),
      (e) => say(messageOf(e)),
    );
  };

  // ---------------------------------------------------------------- the page

  const shownMode: Mode = hover ? "FILE" : mode;
  const fail = (e: unknown) => say(messageOf(e));
  const install_ = () => flowsInstall(shownMode === "FILE" ? file : null, shownMode === "SPEAK", target, keepVoice).catch(fail);
  const toName = languageName(target, languages);
  const fromName = source ? languageName(source, languages) : null;
  // "Choose an audio or video file." is what the empty drop zone already says.
  const problem = plan?.problem && !(shownMode === "FILE" && !file) ? plan.problem : null;
  const noModel = plan != null && plan.modelId == null;

  return (
    <div className="fl-flow">
      <div className="fl-column">
        <div className="fl-head">
          <div className="h5">Translate speech</div>
          <div className="body2 text-secondary">
            Speak, or drop in a recording, a podcast or a video, and hear it in another language, in the same voice where a
            voice can. The text and subtitles in both languages come with it. Everything runs on this computer.
          </div>
        </div>

        <div className="fl-card fl-form">
          <div className="fl-form__modes">
            <div className="nk-kind-switch" role="tablist">
              <ModePill text="Speak" icon="mic" selected={shownMode === "SPEAK"} onClick={() => update({ mode: "SPEAK" })} />
              <ModePill text="Audio or video file" icon="upload" selected={shownMode === "FILE"} onClick={() => update({ mode: "FILE" })} />
            </div>
          </div>

          {shownMode === "SPEAK" ? (
            <MicPanel
              recording={recording}
              ready={ready}
              levels={levels}
              elapsed={recordedAt != null ? now - recordedAt : 0}
              fromName={fromName}
              toName={toName}
              onStart={startRecording}
              onStop={stopRecording}
              onCancel={cancelRecording}
            />
          ) : (
            <DropZone file={file} hover={hover} onChoose={choose} />
          )}

          <div className="fl-form__settings">
            <LanguagePicker label="From" code={source} languages={languages} allowAuto onPick={(c) => update({ source: c })} />
            <IconButton
              icon="swap"
              size={28}
              iconSize={15}
              title={
                source == null
                  ? "Choose the spoken language to swap"
                  : sourceSpoken
                    ? "Swap the languages"
                    : `No voice speaks ${languageName(source, languages)}, so it cannot be translated into`
              }
              disabled={!sourceSpoken || recording !== "IDLE"}
              onClick={() => source && update({ source: target, target: source })}
            />
            <LanguagePicker label="Into" code={target} languages={spoken} allowAuto={false} onPick={(c) => c && update({ target: c })} />
            <span className="nc-flex-spacer" />
            <div className="nk-kind-switch fl-voice" role="tablist" aria-label="Voice">
              <ModePill
                text={shownMode === "SPEAK" ? "My voice" : "Speaker's voice"}
                selected={keepVoice}
                onClick={() => update({ keepVoice: true })}
              />
              <ModePill text="Standard voice" selected={!keepVoice} onClick={() => update({ keepVoice: false })} />
            </div>
          </div>

          {plan && (
            <div className="fl-form__notes">
              <div className={plan.noVoice ? "caption text-warning" : "caption text-tertiary"}>{plan.spokenWith}</div>
              {plan.modelName && (
                <div className="caption text-tertiary fl-form__model">
                  <span>Translated by {plan.modelName}</span>
                  <TextLink text="Change" onClick={onOpenModels} />
                </div>
              )}
            </div>
          )}

          {plan && plan.needs.length > 0 && (
            <DownloadLine
              text={needsText(plan)}
              bytes={plan.totalBytes}
              install={install}
              onDownload={install_}
              onStop={() => flowsCancelInstall().catch(fail)}
              onRetry={() => flowsClearInstallError().then(install_, fail)}
            />
          )}

          {problem && (
            <div className="fl-form__problem">
              <Icon name="alert-circle" size={14} color="var(--warning)" />
              <span className="caption text-warning">{problem}</span>
              {noModel && <TextLink text="Open Models" onClick={onOpenModels} />}
            </div>
          )}

          {shownMode === "FILE" && (
            <div className="fl-form__go">
              <span className="caption text-tertiary">
                {file && ready ? `Translates into ${toName}. You can queue several.` : ""}
              </span>
              <Button text="Translate" icon="translate" iconPosition="start" disabled={!file || !ready} onClick={submit} />
            </div>
          )}
        </div>

        {runs.length > 0 && (
          <div className="fl-runs-head">
            <span className="overline text-tertiary">Translations</span>
            <span className="nc-flex-spacer" />
            <QuietAction text="Open folder" icon="folder-open" onClick={() => flowsOpenFolder(null).catch(() => undefined)} />
          </div>
        )}
        {runs.map((run) => (
          <RunCard
            key={run.id}
            run={run}
            languages={languages}
            onError={say}
            actions={{
              onCancel: () => flowsCancel(run.id).catch(fail),
              onDelete: () => {
                player.stop(run.id);
                flowsDelete(run.id).catch(fail);
              },
              onAgain: () =>
                flowsAgain(run.id).then((r) => {
                  if (r.source === "MICROPHONE") autoplay.current.add(r.id);
                  setRuns((all) => [r, ...all.filter((x) => x.id !== r.id)].sort(newestFirst));
                }, fail),
              // Opening files and folders fails quietly, as the original's runCatching did.
              onOpenFolder: () => flowsOpenFolder(run.id).catch(() => undefined),
              onOpenVideo: () => flowsOpen(run.id, true).catch(() => undefined),
              onCopy: () =>
                navigator.clipboard.writeText(translationText(run)).then(
                  () => say("The translation is copied."),
                  () => say("Could not copy the translation."),
                ),
            }}
          />
        ))}
      </div>
    </div>
  );
}

function ModePill({ text, icon, selected, onClick }: { text: string; icon?: string; selected: boolean; onClick: () => void }) {
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

/** The big round button: press to speak, press again to translate. */
function MicPanel({
  recording,
  ready,
  levels,
  elapsed,
  fromName,
  toName,
  onStart,
  onStop,
  onCancel,
}: {
  recording: Recording;
  ready: boolean;
  levels: number[];
  elapsed: number;
  fromName: string | null;
  toName: string;
  onStart: () => void;
  onStop: () => void;
  onCancel: () => void;
}) {
  if (recording === "RECORDING") {
    return (
      <div className="fl-mic fl-mic--live">
        <button type="button" className="fl-mic__button fl-mic__button--live" title="Stop and translate" aria-label="Stop and translate" onClick={onStop}>
          <Icon name="stop-filled" size={26} />
        </button>
        <WaveformVisualizer amplitudes={levels} width={240} height={32} barColor="var(--error)" barWidth={4} gain={3} />
        <div className="fl-mic__line">
          <span className="numeric">{clock(elapsed)}</span>
          <span className="caption text-tertiary">· Press again when you're done ·</span>
          <TextLink text="Cancel" onClick={onCancel} />
        </div>
      </div>
    );
  }
  const busy = recording !== "IDLE";
  return (
    <div className="fl-mic">
      <button
        type="button"
        className="fl-mic__button"
        disabled={!ready || busy}
        title="Speak"
        aria-label="Speak"
        onClick={onStart}
      >
        {busy ? <Spinner size={24} stroke={2.5} color="var(--on-primary)" trackColor="transparent" /> : <Icon name="mic" size={28} />}
      </button>
      <div className="subtitle2">
        {recording === "SENDING" ? "Sending it to be translated…" : ready ? "Press to speak" : "Set up below to start"}
      </div>
      <div className="caption text-tertiary fl-mic__hint">
        {`Say something${fromName ? ` in ${fromName}` : ""}; Nook says it back in ${toName}. Space starts and stops.`}
      </div>
    </div>
  );
}

const isVideo = (name: string) => /\.(mp4|m4v|mov|mkv|webm|avi|mpg|mpeg|wmv|3gp|ts)$/i.test(name);

/** Where the file lands: dropped from Explorer anywhere on the window, or chosen with the Open dialog. */
function DropZone({ file, hover, onChoose }: { file: string | null; hover: boolean; onChoose: () => void }) {
  const classes = ["fl-drop", hover ? "fl-drop--hover" : "", file && !hover ? "fl-drop--chosen" : ""].filter(Boolean).join(" ");
  if (file && !hover) {
    const { name, folder } = splitPath(file);
    return (
      <div className={classes} role="button" tabIndex={0} onClick={onChoose} onKeyDown={(e) => e.key === "Enter" && onChoose()}>
        <span className="fl-drop__file-icon">
          <Icon name={isVideo(name) ? "video" : "audio-file"} size={20} />
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
      <div className="subtitle2">{hover ? "Drop it to translate it" : "Drop an audio or video file here"}</div>
      <div className="caption text-tertiary">or click to choose one · MP3, M4A, WAV, FLAC, MP4, MKV and more</div>
    </div>
  );
}
