/**
 * The Transcribe Nooklet: a recording (a file dropped or chosen, or the microphone) written down by
 * Whisper, with the time of every line, as text and subtitles; with notes by the chat model (the
 * decisions, the to-dos, the open questions) when asked. The runs follow, newest first.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { onSpeechLevel } from "../../api/code";
import {
  flowsCancelInstall,
  flowsClearInstallError,
  flowsInstallFor,
  flowsLanguages,
  flowsPlanFor,
  flowsRecordCancel,
  flowsRecordStart,
  flowsRecordStopFor,
  flowsSubmitFor,
  TRANSCRIBE,
  type Language,
  type Length,
  type Order,
  type Plan,
} from "../../api/flows";
import { messageOf } from "../../api/ipc";
import { TextLink } from "../../components/Activity";
import { Button } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { Spinner } from "../../components/Spinner";
import { Toggle } from "../../components/Toggle";
import { WaveformVisualizer } from "../code/WaveformVisualizer";
import { DownloadLine } from "./DownloadLine";
import { clock, needsText, splitPath } from "./format";
import { LanguagePicker } from "./LanguagePicker";
import { FileZone, ModePill, NookletRunCard, useFlowRuns } from "./NookletParts";
import { chooseMediaFile, useFileDrop } from "./useFileDrop";

type Mode = "FILE" | "SPEAK";
type Recording = "IDLE" | "STARTING" | "RECORDING" | "SENDING";

interface Choices {
  mode: Mode;
  file: string | null;
  language: string | null;
  notes: boolean;
  length: Length;
}

const STORE = "nook.nooklets.transcribe";

function load(): Choices {
  const fresh: Choices = { mode: "FILE", file: null, language: null, notes: true, length: "short" };
  try {
    const saved = JSON.parse(window.localStorage.getItem(STORE) ?? "null") as Partial<Choices> | null;
    return saved ? { ...fresh, ...saved } : fresh;
  } catch {
    return fresh;
  }
}

function save(c: Choices) {
  try {
    window.localStorage.setItem(STORE, JSON.stringify(c));
  } catch {
    // A convenience only.
  }
}

export function TranscribeFlow({
  say,
  onOpenModels,
  preset = null,
}: {
  say: (message: string) => void;
  onOpenModels: () => void;
  /** A request from the finder ("in German") sets the spoken language. */
  preset?: { language: string; nonce: number } | null;
}) {
  const [languages, setLanguages] = useState<Language[]>([]);
  const [choices, setChoices] = useState<Choices>(load);
  const [plan, setPlan] = useState<Plan | null>(null);
  const [recording, setRecording] = useState<Recording>("IDLE");
  const [levels, setLevels] = useState<number[]>([]);
  const [recordedAt, setRecordedAt] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const { runs, install, planTick, added } = useFlowRuns(TRANSCRIBE, say);
  const recordingRef = useRef(recording);
  recordingRef.current = recording;
  const fail = useCallback((e: unknown) => say(messageOf(e)), [say]);

  const update = useCallback((patch: Partial<Choices>) => {
    setChoices((c) => {
      const next = { ...c, ...patch };
      save(next);
      return next;
    });
  }, []);
  const { mode, file, language, notes, length } = choices;
  const order: Order = { flow: TRANSCRIBE, language, notes, length, focus: null, female: true };

  useEffect(() => {
    flowsLanguages().then(setLanguages, fail);
  }, [fail]);

  const applied = useRef<number | null>(null);
  useEffect(() => {
    if (!preset || applied.current === preset.nonce) return;
    applied.current = preset.nonce;
    update({ language: preset.language });
  }, [preset, update]);

  useEffect(() => {
    let alive = true;
    flowsPlanFor(mode === "FILE" ? file : null, mode === "SPEAK", null, order).then(
      (p) => alive && setPlan(p),
      (e) => alive && fail(e),
    );
    return () => {
      alive = false;
    };
  }, [mode, file, language, notes, length, planTick]);

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
  // Leaving the page drops a recording in progress, or one still starting.
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      if (recordingRef.current === "RECORDING" || recordingRef.current === "STARTING") flowsRecordCancel().catch(() => undefined);
    };
  }, []);

  const ready = plan?.ready === true;
  const start = async () => {
    if (recordingRef.current !== "IDLE" || !ready) return;
    setRecording("STARTING");
    try {
      await flowsRecordStart();
      if (!mounted.current) {
        // The page was left while the microphone started: it must not go on listening.
        flowsRecordCancel().catch(() => undefined);
        return;
      }
      setLevels([]);
      setRecordedAt(Date.now());
      setNow(Date.now());
      setRecording("RECORDING");
    } catch (e) {
      say(messageOf(e) || "The microphone could not start.");
      setRecording("IDLE");
    }
  };
  const stop = async () => {
    if (recordingRef.current !== "RECORDING") return;
    setRecording("SENDING");
    try {
      added(await flowsRecordStopFor(order));
    } catch (e) {
      fail(e);
    } finally {
      setRecording("IDLE");
      setLevels([]);
    }
  };
  const cancel = () => {
    if (recordingRef.current !== "RECORDING") return;
    flowsRecordCancel().catch(() => undefined);
    setRecording("IDLE");
    setLevels([]);
  };

  // ---------------------------------------------------------------- files

  const { hover } = useFileDrop(recording === "IDLE", (path) => update({ mode: "FILE", file: path }));
  const choose = () =>
    chooseMediaFile(file ? splitPath(file).folder : null)
      .then((picked) => picked && update({ mode: "FILE", file: picked }))
      .catch(() => undefined);
  const submit = () => {
    if (!file) return;
    flowsSubmitFor(file, null, order).then(added, fail);
  };
  const download = () => flowsInstallFor(mode === "FILE" ? file : null, mode === "SPEAK", null, order).catch(fail);

  return (
    <div className="fl-flow">
      <div className="fl-column">
        <div className="fl-head">
          <div className="h5">Transcribe a recording</div>
          <div className="body2 text-secondary">
            A meeting, a lecture, an interview or a voice memo, written down with the time of every line, as text and subtitles.
            Ask for notes and the decisions, to-dos and open questions come with it. Nothing leaves this computer.
          </div>
        </div>

        <div className="fl-card fl-form">
          <div className="fl-form__modes">
            <div className="nk-kind-switch" role="tablist">
              <ModePill text="Recording" icon="upload" selected={mode === "FILE"} onClick={() => update({ mode: "FILE" })} />
              <ModePill text="Record now" icon="mic" selected={mode === "SPEAK"} onClick={() => update({ mode: "SPEAK" })} />
            </div>
          </div>

          {mode === "FILE" ? (
            <FileZone file={file} hover={hover} onChoose={choose} icon="audio-file" empty="Drop a recording here" what="MP3, M4A, WAV, FLAC, MP4, MKV and more" />
          ) : recording === "RECORDING" ? (
            <div className="fl-mic fl-mic--live">
              <button type="button" className="fl-mic__button fl-mic__button--live" title="Stop and write it down" aria-label="Stop and write it down" onClick={stop}>
                <Icon name="stop-filled" size={26} />
              </button>
              <WaveformVisualizer amplitudes={levels} width={240} height={32} barColor="var(--error)" barWidth={4} gain={3} />
              <div className="fl-mic__line">
                <span className="numeric">{clock(recordedAt != null ? now - recordedAt : 0)}</span>
                <span className="caption text-tertiary">· Press again when you're done ·</span>
                <TextLink text="Cancel" onClick={cancel} />
              </div>
            </div>
          ) : (
            <div className="fl-mic">
              <button type="button" className="fl-mic__button" disabled={!ready || recording !== "IDLE"} title="Record" aria-label="Record" onClick={start}>
                {recording !== "IDLE" ? (
                  <Spinner size={24} stroke={2.5} color="var(--on-primary)" trackColor="transparent" />
                ) : (
                  <Icon name="mic" size={28} />
                )}
              </button>
              <div className="subtitle2">{recording === "SENDING" ? "Sending it to be written down…" : ready ? "Press to record" : "Set up below to start"}</div>
              <div className="caption text-tertiary fl-mic__hint">A meeting in the room, a thought, a dictated letter: Nook writes it down when you stop.</div>
            </div>
          )}

          <div className="fl-form__settings">
            <LanguagePicker label="Spoken in" code={language} languages={languages} allowAuto onPick={(c) => update({ language: c })} />
            <span className="nc-flex-spacer" />
            <span className="cv-option">
              <Toggle checked={notes} onChange={(v) => update({ notes: v })} label="Write notes" />
              <span className="body2">Notes</span>
            </span>
            {notes && (
              <div className="nk-kind-switch fl-voice" role="tablist" aria-label="Length of the notes">
                <ModePill text="Short" selected={length === "short"} onClick={() => update({ length: "short" })} />
                <ModePill text="Detailed" selected={length === "detailed"} onClick={() => update({ length: "detailed" })} />
              </div>
            )}
          </div>

          {plan && (
            <div className="fl-form__notes">
              <div className="caption text-tertiary fl-form__model">
                <span>{plan.spokenWith}</span>
                {plan.modelName && <TextLink text="Change" onClick={onOpenModels} />}
              </div>
            </div>
          )}

          {plan && plan.needs.length > 0 && (
            <DownloadLine
              text={needsText(plan)}
              bytes={plan.totalBytes}
              install={install}
              onDownload={download}
              onStop={() => flowsCancelInstall().catch(fail)}
              onRetry={() => flowsClearInstallError().then(download, fail)}
            />
          )}

          {plan?.problem && !(mode === "FILE" && !file) && (
            <div className="fl-form__problem">
              <Icon name="alert-circle" size={14} color="var(--warning)" />
              <span className="caption text-warning">{plan.problem}</span>
              {plan.problem.includes("Settings") && <TextLink text="Open Models" onClick={onOpenModels} />}
            </div>
          )}

          {mode === "FILE" && (
            <div className="fl-form__go">
              <span className="caption text-tertiary">{file && ready ? "You can queue several." : ""}</span>
              <Button text="Transcribe" icon="transcribe" iconPosition="start" disabled={!file || !ready} onClick={submit} />
            </div>
          )}
        </div>

        {runs.length > 0 && (
          <div className="fl-runs-head">
            <span className="overline text-tertiary">Transcripts</span>
          </div>
        )}
        {runs.map((run) => (
          <NookletRunCard key={run.id} run={run} onError={say} />
        ))}
      </div>
    </div>
  );
}
