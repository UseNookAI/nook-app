/**
 * The Summarize Nooklet: a document (a PDF, scans too, Word, a web page, an e-book...) or pasted
 * text, summarized by the chat model: a paragraph and the key points, or a section for each part,
 * with what is worth checking (deadlines, amounts, obligations). A long document is read in parts.
 * The summary can be written in another language, and a question says what matters most.
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
  SUMMARIZE,
  type Language,
  type Length,
  type Order,
  type Peek,
  type Plan,
} from "../../api/flows";
import { messageOf } from "../../api/ipc";
import { TextLink } from "../../components/Activity";
import { Button } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { DownloadLine } from "./DownloadLine";
import { needsText, wordsText } from "./format";
import { LanguagePicker } from "./LanguagePicker";
import { chooseDocument, DocumentInput, ModePill, NookletRunCard, useFlowRuns } from "./NookletParts";
import { useFileDrop } from "./useFileDrop";

interface Choices {
  mode: "FILE" | "TEXT";
  file: string | null;
  language: string | null;
  length: Length;
}

const STORE = "nook.nooklets.summarize";
/** Pasted text, kept while another page is open (not across restarts). */
let pasted = "";
let focusKept = "";

function load(): Choices {
  const fresh: Choices = { mode: "FILE", file: null, language: null, length: "short" };
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

export function SummarizeFlow({
  say,
  onOpenModels,
  preset = null,
}: {
  say: (message: string) => void;
  onOpenModels: () => void;
  /** A request from the finder ("in English") sets the summary's language. */
  preset?: { language: string; nonce: number } | null;
}) {
  const [languages, setLanguages] = useState<Language[]>([]);
  const [choices, setChoices] = useState<Choices>(load);
  const [text, setTextState] = useState(pasted);
  const [focus, setFocusState] = useState(focusKept);
  const [plan, setPlan] = useState<Plan | null>(null);
  const [peek, setPeek] = useState<Peek | null>(null);
  const { runs, install, planTick, added } = useFlowRuns(SUMMARIZE, say);
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
  const setFocus = (f: string) => {
    focusKept = f;
    setFocusState(f);
  };
  const { mode, file, language, length } = choices;
  const input = mode === "FILE" ? file : null;
  const typed = mode === "TEXT" ? text : null;
  const order: Order = { flow: SUMMARIZE, language, notes: false, length, focus: focus.trim() || null, female: true };

  useEffect(() => {
    flowsLanguages().then(setLanguages, fail);
  }, [fail]);

  const applied = useRef<number | null>(null);
  useEffect(() => {
    if (!preset || applied.current === preset.nonce) return;
    applied.current = preset.nonce;
    update({ language: preset.language });
  }, [preset, update]);

  // The plan does not depend on the text itself, only on whether there is some.
  const hasText = (typed ?? "").trim() !== "";
  useEffect(() => {
    let alive = true;
    flowsPlanFor(input, false, hasText ? "text" : null, order).then(
      (p) => alive && setPlan(p),
      (e) => alive && fail(e),
    );
    return () => {
      alive = false;
    };
  }, [input, hasText, language, length, planTick]);

  // How long the document is, once it is chosen.
  useEffect(() => {
    setPeek(null);
    if (mode !== "FILE" || !file) return;
    let alive = true;
    flowsPeek(file, null).then((p) => alive && setPeek(p), () => undefined);
    return () => {
      alive = false;
    };
  }, [mode, file, planTick]);

  const { hover } = useFileDrop(true, (path) => update({ mode: "FILE", file: path }));
  const choose = () =>
    chooseDocument("Choose a document to summarize")
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
  const words = mode === "TEXT" ? (hasText ? text.trim().split(/\s+/).length : 0) : (peek?.words ?? 0);

  return (
    <div className="fl-flow">
      <div className="fl-column">
        <div className="fl-head">
          <div className="h5">Summarize a document</div>
          <div className="body2 text-secondary">
            A contract, a report, a paper, a long article or a letter: the key points and what is worth checking, in a minute
            instead of an hour. A long document is read in parts. Private papers stay private: it all runs on this computer.
          </div>
        </div>

        <div className="fl-card fl-form">
          <DocumentInput
            mode={mode}
            file={file}
            text={text}
            hover={hover}
            placeholder="Paste the text to summarize here"
            onMode={(m) => update({ mode: m })}
            onChoose={choose}
            onText={setText}
          />

          <input
            className="nl-focus body2"
            value={focus}
            onChange={(e) => setFocus(e.target.value)}
            placeholder="Anything to look out for? (optional) e.g. the notice period, the costs"
            aria-label="What matters most"
          />

          <div className="fl-form__settings">
            <div className="nk-kind-switch fl-voice" role="tablist" aria-label="Length">
              <ModePill text="Short" selected={length === "short"} onClick={() => update({ length: "short" })} />
              <ModePill text="Detailed" selected={length === "detailed"} onClick={() => update({ length: "detailed" })} />
            </div>
            <span className="nc-flex-spacer" />
            <LanguagePicker
              label="Write in"
              code={language}
              languages={languages}
              allowAuto
              autoText="The document's language"
              onPick={(c) => update({ language: c })}
            />
          </div>

          {plan && (
            <div className="fl-form__notes">
              <div className="caption text-tertiary fl-form__model">
                <span>
                  {words > 0 ? `${wordsText(words)}. ` : ""}
                  {plan.spokenWith}
                </span>
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

          {plan?.problem && (mode === "FILE" ? file != null : hasText) && (
            <div className="fl-form__problem">
              <Icon name="alert-circle" size={14} color="var(--warning)" />
              <span className="caption text-warning">{plan.problem}</span>
              {plan.problem.includes("Settings") && <TextLink text="Open Models" onClick={onOpenModels} />}
            </div>
          )}

          <div className="fl-form__go">
            <span className="caption text-tertiary">{ready ? "You can queue several." : ""}</span>
            <Button text="Summarize" icon="summarize" iconPosition="start" disabled={!ready} onClick={submit} />
          </div>
        </div>

        {runs.length > 0 && (
          <div className="fl-runs-head">
            <span className="overline text-tertiary">Summaries</span>
          </div>
        )}
        {runs.map((run) => (
          <NookletRunCard key={run.id} run={run} onError={say} />
        ))}
      </div>
    </div>
  );
}
