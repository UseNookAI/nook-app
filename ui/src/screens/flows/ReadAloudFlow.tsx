/**
 * The Read aloud Nooklet: a document or pasted text read by a natural voice, line after line, as
 * one track to play here or take along (an M4A with FFmpeg in, else a WAV): an audiobook of a
 * chapter, a report to hear on the way, a voiceover. The text's language is told from the text,
 * and the voice can be a woman's or a man's. A finished reading plays with its text lit line by
 * line.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import {
  flowsCancelInstall,
  flowsClearInstallError,
  flowsInstallFor,
  flowsLanguages,
  flowsPeek,
  flowsPlanFor,
  flowsSubmitFor,
  READ_ALOUD,
  type Language,
  type Order,
  type Peek,
  type Plan,
} from "../../api/flows";
import { messageOf } from "../../api/ipc";
import { Button } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { DownloadLine } from "./DownloadLine";
import { listenText, needsText, wordsText } from "./format";
import { LanguagePicker } from "./LanguagePicker";
import { chooseDocument, DocumentInput, ModePill, NookletRunCard, useFlowRuns } from "./NookletParts";
import { useFileDrop } from "./useFileDrop";

interface Choices {
  mode: "FILE" | "TEXT";
  file: string | null;
  language: string | null;
  female: boolean;
}

const STORE = "nook.nooklets.read-aloud";
let pasted = "";

function load(): Choices {
  const fresh: Choices = { mode: "FILE", file: null, language: null, female: true };
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

export function ReadAloudFlow({
  say,
  preset = null,
}: {
  say: (message: string) => void;
  /** A request from the finder ("in French") sets the text's language. */
  preset?: { language: string; nonce: number } | null;
}) {
  const [languages, setLanguages] = useState<Language[]>([]);
  const [choices, setChoices] = useState<Choices>(load);
  const [text, setTextState] = useState(pasted);
  const [plan, setPlan] = useState<Plan | null>(null);
  const [peek, setPeek] = useState<Peek | null>(null);
  const [peeking, setPeeking] = useState(false);
  const { runs, install, planTick, added } = useFlowRuns(READ_ALOUD, say);
  const fail = useCallback((e: unknown) => say(messageOf(e)), [say]);

  const update = useCallback((patch: Partial<Choices>) => {
    setChoices((c) => {
      const next = { ...c, ...patch };
      save(next);
      return next;
    });
  }, []);
  const setText = (t: string) => {
    pasted = t;
    setTextState(t);
  };
  const { mode, file, female } = choices;
  const spoken = languages.filter((l) => l.spoken !== false);
  // At first the computer's own language (the translator's default is another one on purpose).
  const own = (navigator.language ?? "en").split(/[-_]/)[0].toLowerCase();
  const language = choices.language ?? (spoken.some((l) => l.code === own) ? own : "en");
  const input = mode === "FILE" ? file : null;
  const typed = mode === "TEXT" ? text : null;
  const hasText = (typed ?? "").trim() !== "";
  const order: Order = { flow: READ_ALOUD, language, notes: false, length: "short", focus: null, female };

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
    flowsPlanFor(input, false, hasText ? "text" : null, order).then(
      (p) => alive && setPlan(p),
      (e) => alive && fail(e),
    );
    return () => {
      alive = false;
    };
  }, [input, hasText, language, female, planTick]);

  // The text's language and length, once there is a text: the language picks itself.
  useEffect(() => {
    setPeek(null);
    const source = mode === "FILE" ? file : text.trim() ? text : null;
    if (!source) return;
    let alive = true;
    const timer = window.setTimeout(() => {
      setPeeking(true);
      flowsPeek(mode === "FILE" ? file : null, mode === "TEXT" ? text : null)
        .then((p) => {
          if (!alive) return;
          setPeek(p);
          if (p.language) update({ language: p.language });
        }, () => undefined)
        .finally(() => alive && setPeeking(false));
    }, mode === "TEXT" ? 700 : 0);
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [mode, file, text, planTick, update]);

  const { hover } = useFileDrop(true, (path) => update({ mode: "FILE", file: path }));
  const choose = () =>
    chooseDocument("Choose a document to read aloud")
      .then((picked) => picked && update({ mode: "FILE", file: picked }))
      .catch(() => undefined);
  const ready = plan?.ready === true && (mode === "FILE" ? file != null : hasText);
  const submit = () => {
    flowsSubmitFor(input, typed, order).then((run) => {
      added(run);
      if (mode === "TEXT") setText("");
    }, fail);
  };
  const download = () => flowsInstallFor(input, false, typed, order).catch(fail);
  const words = peek?.words ?? 0;

  return (
    <div className="fl-flow">
      <div className="fl-column">
        <div className="fl-head">
          <div className="h5">Read it aloud</div>
          <div className="body2 text-secondary">
            A chapter, a report, an article or your own script, read to you by a natural voice: listen on the way, rest your
            eyes, or make a voiceover. You get one track to play here or take along. It all runs on this computer.
          </div>
        </div>

        <div className="fl-card fl-form">
          <DocumentInput
            mode={mode}
            file={file}
            text={text}
            hover={hover}
            placeholder="Paste the text to read aloud here"
            onMode={(m) => update({ mode: m })}
            onChoose={choose}
            onText={setText}
          />

          <div className="fl-form__settings">
            <LanguagePicker label="Written in" code={language} languages={spoken} allowAuto={false} onPick={(c) => c && update({ language: c })} />
            <span className="nc-flex-spacer" />
            <div className="nk-kind-switch fl-voice" role="tablist" aria-label="Voice">
              <ModePill text="Woman's voice" selected={female} onClick={() => update({ female: true })} />
              <ModePill text="Man's voice" selected={!female} onClick={() => update({ female: false })} />
            </div>
          </div>

          {plan && (
            <div className="fl-form__notes">
              <div className={plan.noVoice ? "caption text-warning" : "caption text-tertiary"}>
                {peeking ? "Reading the text… " : words > 0 ? `${wordsText(words)}, ${listenText(words)} to listen. ` : ""}
                {plan.spokenWith}
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

          {plan?.problem && (mode === "FILE" ? file != null : hasText) && (
            <div className="fl-form__problem">
              <Icon name="alert-circle" size={14} color="var(--warning)" />
              <span className="caption text-warning">{plan.problem}</span>
            </div>
          )}

          <div className="fl-form__go">
            <span className="caption text-tertiary">{ready ? "You can queue several." : ""}</span>
            <Button text="Read aloud" icon="read-aloud" iconPosition="start" disabled={!ready} onClick={submit} />
          </div>
        </div>

        {runs.length > 0 && (
          <div className="fl-runs-head">
            <span className="overline text-tertiary">Readings</span>
          </div>
        )}
        {runs.map((run) => (
          <NookletRunCard key={run.id} run={run} onError={say} />
        ))}
      </div>
    </div>
  );
}
