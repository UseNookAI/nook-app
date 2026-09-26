/**
 * VideoScreen.kt: the video tool: write a prompt, and the local GPU renders a short clip. Clips
 * queue and run one at a time; finished ones stay in the videos folder and play here in a loop.
 */
import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type RefObject } from "react";
import { messageOf, on } from "../../api/ipc";
import {
  videoCancel,
  videoClips,
  videoDelete,
  videoDownload,
  videoDownloadPause,
  videoDownloadResume,
  videoDownloadState,
  videoOpen,
  videoOpenFolder,
  videoReveal,
  videoSetup,
  videoSubmit,
  type Clip,
  type DownloadState,
  type VideoSetup,
} from "../../api/video";
import { QuietAction } from "../../components/Activity";
import { Button } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { Menu } from "../../components/Menu";
import { ProgressBar } from "../../components/Spinner";
import { PromptActionIconButton } from "../hub/ComposerControls";
import { KindSwitch } from "../hub/KindSwitch";
import { PagePane } from "../hub/PagePane";
import { Card, ClipCard } from "./ClipCard";
import { clipText, gigabytes } from "./format";
import "./video.css";

export interface VideoScreenProps {
  /** Shows a snackbar message. */
  say: (message: string) => void;
  /** Back to the Chat start page (the Chat | Video switch). */
  onChat: () => void;
}

/** What the page can ask for. Every call returns at once; the work happens elsewhere. */
export interface VideoActions {
  submit: (prompt: string, modelId: string | null) => void;
  cancel: (id: string) => void;
  delete: (id: string) => void;
  open: (id: string) => void;
  reveal: (id: string) => void;
  openFolder: () => void;
  download: () => void;
  pause: () => void;
  resume: () => void;
}

const NO_DOWNLOAD: DownloadState = { offered: false, downloading: false, paused: false, progress: null };

/** Feeds `VideoPage` from the studio and the downloads. */
export function VideoScreen({ say, onChat }: VideoScreenProps) {
  const [clips, setClips] = useState<Clip[]>([]);
  const [setup, setSetup] = useState<VideoSetup | null>(null);
  const [download, setDownload] = useState<DownloadState>(NO_DOWNLOAD);
  /** The model picked in the header; null = the preferred installed one, which the original always used. */
  const [modelId, setModelId] = useState<string | null>(null);
  // `say` may be a new function on every render of the parent; the subscriptions keep the latest.
  const sayRef = useRef(say);
  useEffect(() => {
    sayRef.current = say;
  }, [say]);

  useEffect(() => {
    let alive = true;
    // Changes come from the render thread; the list is replaced whole each time.
    const read = () =>
      videoClips().then(
        (c) => alive && setClips(c),
        (e) => alive && sayRef.current(messageOf(e)),
      );
    read();
    const off = on("video", read);
    return () => {
      alive = false;
      off();
    };
  }, []);

  // Read again when a download moves or finishes, so the page opens up as soon as the model is in.
  useEffect(() => {
    let alive = true;
    const read = async () => {
      try {
        const s = await videoSetup(modelId);
        if (!alive) return;
        setSetup(s);
        const d = s.modelId ? await videoDownloadState(s.modelId) : NO_DOWNLOAD;
        if (alive) setDownload(d);
      } catch (e) {
        if (alive) sayRef.current(messageOf(e));
      }
    };
    read();
    const off = on("downloads", read);
    return () => {
      alive = false;
      off();
    };
  }, [modelId]);

  const catalogId = setup?.modelId ?? null;
  const fail = (e: unknown) => say(messageOf(e));
  const actions: VideoActions = {
    submit: (prompt, model) => void videoSubmit(prompt, model).catch(fail),
    cancel: (id) => void videoCancel(id).catch(fail),
    delete: (id) => void videoDelete(id).catch(fail),
    // Opening files and folders fails quietly, as the original's runCatching did.
    open: (id) => void videoOpen(id).catch(() => undefined),
    reveal: (id) => void videoReveal(id).catch(() => undefined),
    openFolder: () => void videoOpenFolder().catch(() => undefined),
    download: () => void (catalogId && videoDownload(catalogId).catch(fail)),
    pause: () => void (catalogId && videoDownloadPause(catalogId).catch(fail)),
    resume: () => void (catalogId && videoDownloadResume(catalogId).catch(fail)),
  };
  return (
    <VideoPage
      setup={setup}
      clips={clips}
      download={download}
      actions={actions}
      modelId={modelId}
      onModel={setModelId}
      onChat={onChat}
    />
  );
}

/** The composer's placeholder: one scene, with what moves and how it looks. */
const EXAMPLE_PROMPT = "A fox runs through fresh snow at dawn, slow motion, soft light";

/** The page itself, from plain state: `VideoScreen` feeds it from the studio and the downloads. */
export function VideoPage({
  setup,
  clips,
  download,
  actions,
  modelId,
  onModel,
  onChat,
}: {
  setup: VideoSetup | null;
  clips: Clip[];
  download: DownloadState;
  actions: VideoActions;
  modelId: string | null;
  onModel: (id: string) => void;
  onChat: () => void;
}) {
  const [text, setText] = useState("");
  const composer = useRef<HTMLTextAreaElement>(null);
  const ready = setup != null && setup.problem == null;
  const newestDone = clips.find((c) => c.status === "DONE")?.id;

  const send = () => {
    actions.submit(text, modelId);
    setText("");
  };
  const reuse = (prompt: string) => {
    setText(prompt);
    // The caret goes to the end, as the original's TextRange(prompt.length).
    requestAnimationFrame(() => {
      const el = composer.current;
      if (!el) return;
      el.focus();
      el.setSelectionRange(prompt.length, prompt.length);
    });
  };

  return (
    <PagePane>
      <div className="vd-topbar">
        <KindSwitch kind="VIDEO" onKind={(k) => k === "CHAT" && onChat()} />
        {setup && setup.problem == null && <ModelLine setup={setup} modelId={modelId} onModel={onModel} />}
        <div className="vd-grow" />
        <QuietAction text="Open folder" icon="folder-open" title={setup?.folder} onClick={actions.openFolder} />
      </div>
      <div className="vd-list">
        <VideoComposer inputRef={composer} text={text} onText={setText} enabled={ready} onSend={send} />
        {setup?.problem != null ? (
          <SetupCard setup={setup} download={download} actions={actions} />
        ) : (
          setup != null &&
          clips.length === 0 && (
            <div className="vd-empty body2 text-tertiary">
              Describe a scene: what is in it, what happens, and how it looks. A clip takes a few minutes on the GPU; you can
              queue several.
            </div>
          )
        )}
        {clips.map((clip) => (
          <ClipCard
            key={clip.id}
            clip={clip}
            autoPlay={clip.id === newestDone}
            actions={{
              onCancel: () => actions.cancel(clip.id),
              onDelete: () => actions.delete(clip.id),
              onRetry: () => actions.submit(clip.prompt, clip.modelId),
              onReuse: () => reuse(clip.prompt),
              onOpen: () => actions.open(clip.id),
              onReveal: () => actions.reveal(clip.id),
            }}
          />
        ))}
      </div>
    </PagePane>
  );
}

/**
 * The header's "Wan 2.1 T2V 1.3B · 832×480 · 2.1 s clips". With more than one video model
 * installed the name opens a menu to pick the one the next clips use.
 */
function ModelLine({ setup, modelId, onModel }: { setup: VideoSetup; modelId: string | null; onModel: (id: string) => void }) {
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const line = `${setup.modelName} · ${clipText(setup)}`;
  if (setup.models.length < 2) return <span className="vd-model caption text-tertiary">{line}</span>;
  const current = modelId ?? setup.modelId;
  return (
    <>
      <button
        type="button"
        className="vd-model vd-model--choice caption"
        title="Choose the video model"
        onClick={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          setMenu({ x: r.left, y: r.bottom + 4 });
        }}
      >
        <span className="vd-model__text">{line}</span>
        <Icon name="arrow-down" size={12} />
      </button>
      {menu && (
        <Menu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={setup.models.map((m) => ({
            label: m.name,
            // Every row has an icon so the names line up; the tick marks the one in use.
            icon: m.id === current ? "check" : "video",
            onSelect: () => onModel(m.id),
          }))}
        />
      )}
    </>
  );
}

function VideoComposer({
  inputRef,
  text,
  onText,
  enabled,
  onSend,
}: {
  inputRef: RefObject<HTMLTextAreaElement | null>;
  text: string;
  onText: (text: string) => void;
  enabled: boolean;
  onSend: () => void;
}) {
  const canSend = enabled && text.trim().length > 0;
  // Grows with the text (or the placeholder, as the Compose box did) up to the prompt height, then
  // scrolls; measured again when the width changes and the lines wrap differently.
  useLayoutEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    const fit = () => {
      el.style.height = "0px";
      el.style.height = `${el.scrollHeight}px`;
    };
    fit();
    let width = el.clientWidth;
    const observer = new ResizeObserver(() => {
      if (el.clientWidth === width) return;
      width = el.clientWidth;
      fit();
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [text, inputRef]);
  const key = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
    // Shift+Enter is a new line; Enter sends.
    if (e.shiftKey) return;
    e.preventDefault();
    if (canSend) onSend();
  };
  return (
    <div className="nk-composer">
      <div className="vd-composer__field">
        <textarea
          ref={inputRef}
          className="vd-composer__input body1"
          value={text}
          rows={1}
          disabled={!enabled}
          placeholder={enabled ? EXAMPLE_PROMPT : "Download the video model to start"}
          onChange={(e) => onText(e.target.value)}
          onKeyDown={key}
        />
      </div>
      <div className="vd-composer__row">
        <div className="vd-grow" />
        <PromptActionIconButton isGenerating={false} isEnabled={canSend} onSend={onSend} onCancel={() => undefined} />
      </div>
    </div>
  );
}

/** No model or no engine yet: what the download is, and the button (or its progress). */
function SetupCard({ setup, download, actions }: { setup: VideoSetup; download: DownloadState; actions: VideoActions }) {
  const progress = download.progress ?? 0;
  return (
    <Card className="vd-setup">
      <div className="subtitle2">Set up video</div>
      <div className="body2 text-secondary">
        Clips are made on this computer's GPU with {setup.modelName}. It needs a one-time download of{" "}
        {gigabytes(setup.downloadBytes)}: the model and its text encoder, and the video engine if it is not installed yet. An 8 GB
        graphics card is enough.
      </div>
      {download.downloading || download.paused ? (
        <>
          <div className="vd-track">
            <ProgressBar progress={progress} height={8} />
          </div>
          <div className="vd-setup__status">
            <span className="caption text-tertiary vd-grow">
              {(download.paused ? "Paused at " : "Downloading… ") + `${(progress * 100).toFixed(0)}%`}
            </span>
            {download.paused ? (
              <QuietAction text="Resume" icon="resume" onClick={actions.resume} />
            ) : (
              <QuietAction text="Pause" icon="pause" onClick={actions.pause} />
            )}
          </div>
        </>
      ) : download.offered ? (
        <div className="vd-row">
          <Button
            text={`Download (${gigabytes(setup.downloadBytes)})`}
            icon="download"
            iconPosition="start"
            onClick={actions.download}
          />
        </div>
      ) : (
        <div className="caption text-warning">{setup.problem ?? ""}</div>
      )}
    </Card>
  );
}
