/**
 * The Nooklets' front door: Scout, one question and a box to answer it in. The request goes to
 * the finder (a small model on the processor that reads any language), which answers with the
 * Nooklet for the job and what the request sets for it ("into German", "to PDF"); opening it
 * applies that. Enter asks; Enter again, the request unchanged, opens the answer.
 *
 * Until the finder is downloaded the request's words choose, and a line offers the download.
 */
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { Install } from "../../api/flows";
import { messageOf } from "../../api/ipc";
import {
  nookletsCancelInstall,
  nookletsClearInstallError,
  nookletsFind,
  nookletsInstall,
  nookletsSetup,
  onNooklets,
  type FinderSetup,
  type Found,
  type Hit,
  type NookletId,
  type Preset,
} from "../../api/nooklets";
import { Icon } from "../../components/Icon";
import { PromptActionIconButton } from "../hub/ComposerControls";
import { SpeechButton } from "../code/CodeModels";
import { DownloadLine } from "./DownloadLine";
import { Scout, type Mood } from "./Scout";
import "../code/code.css";

/** The Nooklets there are, for the icons and the "or pick one" row. */
export const NOOKLET_ICONS: Record<NookletId, string> = { translate: "translate", pdf: "file-edit", convert: "file-convert" };
const ALL: { id: NookletId; title: string }[] = [
  { id: "translate", title: "Translate speech" },
  { id: "pdf", title: "Edit a PDF" },
  { id: "convert", title: "Convert documents" },
];

const EXAMPLES = [
  "Turn my Word file into a PDF",
  "Change the date on this scanned form",
  "Translate what I say into Spanish",
  "Put these photos into one PDF",
  "Make a CSV out of this Excel sheet",
  "Dub this video in German",
];

/** The request and its answer, kept while another page is open. */
const kept = { text: "", asked: "", found: null as Found | null };

const HEIGHT_PROMPT = 160;

export function NookletsHome({ say, onOpen }: { say: (message: string) => void; onOpen: (id: NookletId, preset: Preset | null) => void }) {
  const [text, setTextState] = useState(kept.text);
  const [asked, setAsked] = useState(kept.asked);
  const [found, setFound] = useState<Found | null>(kept.found);
  const [searching, setSearching] = useState(false);
  const [setup, setSetup] = useState<FinderSetup | null>(null);
  const [install, setInstall] = useState<Install | null>(null);
  const [example, setExample] = useState(0);
  /** Bumped by every answer, so Scout hops again. */
  const [answers, setAnswers] = useState(0);
  const box = useRef<HTMLTextAreaElement>(null);
  const sayRef = useRef(say);
  sayRef.current = say;
  const fail = useCallback((e: unknown) => sayRef.current(messageOf(e)), []);

  const setText = (next: string) => {
    kept.text = next;
    setTextState(next);
  };

  // Grows with the text, then scrolls.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, HEIGHT_PROMPT)}px`;
  }, [text]);

  useEffect(() => {
    box.current?.focus();
  }, []);

  // The placeholder shows what can be asked, one example after another.
  useEffect(() => {
    const timer = window.setInterval(() => setExample((i) => (i + 1) % EXAMPLES.length), 3500);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    let alive = true;
    const read = () => nookletsSetup().then((s) => alive && (setSetup(s), setInstall(s.install)), () => undefined);
    read();
    const off = onNooklets((e) => {
      if (e.install === undefined) return;
      setInstall(e.install);
      if (e.install == null) read();
    });
    return () => {
      alive = false;
      off();
    };
  }, []);

  const request = text.trim();
  const answered = found != null && asked === request;

  const ask = () => {
    if (!request || searching) return;
    if (answered && found.matched) {
      const top = found.hits[0];
      onOpen(top.id, top.preset);
      return;
    }
    setSearching(true);
    nookletsFind(request)
      .then((f) => {
        kept.found = f;
        kept.asked = request;
        setFound(f);
        setAsked(request);
        setAnswers((n) => n + 1);
      }, fail)
      .finally(() => setSearching(false));
  };

  const mood: Mood = searching ? "searching" : answered ? (found.matched ? "found" : "lost") : "idle";
  const top = found?.hits[0];
  const others = found?.matched ? found.hits.slice(1).filter((h) => h.fits) : [];

  return (
    <div className="nl-home">
      <div className="nl-home__column">
        <Scout key={mood === "found" ? `found-${answers}` : mood} mood={mood} size={150} />
        <div className="h4 nl-home__title">What do you want done?</div>
        <div className="body2 text-secondary nl-home__blurb">
          Say it in your own words, in any language. Nook finds the Nooklet for the job.
        </div>

        <div className="nk-composer nc-composer">
          <textarea
            ref={box}
            className="nc-composer__input body1"
            value={text}
            rows={1}
            aria-label="What do you want done?"
            placeholder={EXAMPLES[example]}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key !== "Enter" || e.nativeEvent.isComposing || e.shiftKey) return;
              e.preventDefault();
              ask();
            }}
          />
          <div className="nc-composer__row">
            <span className="caption text-tertiary nl-home__hint">
              {answered && found.matched ? `Enter opens ${top!.title}` : found?.by === "words" && answered ? "Matched by its words" : ""}
            </span>
            <SpeechButton
              enabled={!searching}
              onText={(said) => {
                const joined = text.trim() === "" ? said : text.trimEnd() + " " + said;
                setText(joined);
                requestAnimationFrame(() => {
                  const el = box.current;
                  if (el) {
                    el.focus();
                    el.setSelectionRange(joined.length, joined.length);
                  }
                });
              }}
              onError={(m) => sayRef.current(m)}
            />
            <PromptActionIconButton isGenerating={false} isLoading={searching} isEnabled={request !== ""} onSend={ask} onCancel={() => undefined} />
          </div>
        </div>

        {found && (
          <div className="nl-results" aria-live="polite">
            {found.matched ? (
              <>
                <div className="nl-results__head">
                  <span className="overline text-tertiary">{found.sure ? "The Nooklet for it" : "This one may do it"}</span>
                </div>
                <HitCard key={`${answers}-${top!.id}`} hit={top!} best onOpen={onOpen} />
                {others.length > 0 && (
                  <div className="nl-results__head">
                    <span className="overline text-tertiary">Or</span>
                  </div>
                )}
                {others.map((h) => (
                  <HitCard key={`${answers}-${h.id}`} hit={h} onOpen={onOpen} />
                ))}
              </>
            ) : (
              <>
                <div className="nl-results__head">
                  <span className="body2 text-secondary">No Nooklet does that yet. These are the ones there are:</span>
                </div>
                {found.hits.map((h) => (
                  <HitCard key={`${answers}-${h.id}`} hit={{ ...h, preset: null }} onOpen={onOpen} />
                ))}
              </>
            )}
          </div>
        )}

        {setup && !setup.installed && (
          <div className="nl-home__setup">
            <DownloadLine
              text="One-time download: the finder, a small model that runs on any processor and understands requests in any language. Until then, Nook matches the words."
              bytes={setup.bytes}
              install={install}
              onDownload={() => nookletsInstall().catch(fail)}
              onStop={() => nookletsCancelInstall().catch(fail)}
              onRetry={() => nookletsClearInstallError().then(() => nookletsInstall(), fail)}
            />
          </div>
        )}

        {!found && (
          <div className="nl-home__all">
            <span className="caption text-tertiary">or open one yourself</span>
            {ALL.map((n) => (
              <button key={n.id} type="button" className="nl-pick body2" onClick={() => onOpen(n.id, null)}>
                <Icon name={NOOKLET_ICONS[n.id]} size={15} />
                {n.title}
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function HitCard({ hit, best = false, onOpen }: { hit: Hit; best?: boolean; onOpen: (id: NookletId, preset: Preset | null) => void }) {
  return (
    <button type="button" className={best ? "nl-hit nl-hit--best" : "nl-hit"} onClick={() => onOpen(hit.id, hit.preset)}>
      <span className="nl-hit__icon">
        <Icon name={NOOKLET_ICONS[hit.id]} size={20} />
      </span>
      <span className="nl-hit__text">
        <span className="nl-hit__title">
          <span className="subtitle1">{hit.title}</span>
          {hit.preset && <span className="nk-chip nk-chip--accent">{hit.preset.key === "language" ? `into ${hit.preset.label}` : `to ${hit.preset.label}`}</span>}
        </span>
        <span className="caption text-secondary">{hit.blurb}</span>
      </span>
      <span className="nl-hit__go body2">
        Open
        <Icon name="arrow-right-no-line" size={14} />
      </span>
    </button>
  );
}
