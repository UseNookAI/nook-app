/**
 * The screen recorder Nooklet: records or streams a whole screen, one window (wherever it goes,
 * even behind others) or an area dragged out over the screen, with a microphone and the
 * computer's own sound, each with its meter. What it records shows as a picture before it
 * starts. While it runs, the time, the size and the frame rate count up here and on the small
 * controls it puts on the recorded screen (kept out of the recording); stopping makes one MP4 in
 * the chosen folder, to open or show. FFmpeg does the work and downloads the first time.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  captureAreaPicked,
  captureCancelInstall,
  captureClearInstallError,
  captureInstall,
  captureInstallState,
  captureKeepStreamKey,
  captureListen,
  captureOpen,
  captureOpenFolder,
  capturePause,
  capturePickArea,
  capturePreview,
  captureResume,
  captureReveal,
  captureSources,
  captureStart,
  captureState,
  captureStop,
  captureStopListening,
  captureStreamKey,
  chooseRecordingFolder,
  clock,
  IDLE,
  onCapture,
  SERVICES,
  sourceText,
  type AudioSource,
  type CaptureState,
  type Quality,
  type Source,
  type Sources,
  type StreamService,
} from "../../api/capture";
import type { Install } from "../../api/flows";
import { messageOf } from "../../api/ipc";
import { QuietAction, TextLink } from "../../components/Activity";
import { Button, IconButton } from "../../components/Button";
import { Dropdown } from "../../components/Dropdown";
import { Icon } from "../../components/Icon";
import { Spinner } from "../../components/Spinner";
import { TextField } from "../../components/TextField";
import { Toggle } from "../../components/Toggle";
import { DownloadLine } from "./DownloadLine";
import { bytesText } from "./format";
import { ModePill } from "./NookletParts";
import "./screen.css";
import { isMac } from "../../shell/platform";

type Mode = "record" | "stream";
type Kind = "screen" | "window" | "area";
type Size = "source" | "1080" | "720";

/** What the form keeps between visits, in this browser's storage (never the stream key). */
interface Kept {
  mode: Mode;
  kind: Kind;
  fps: number;
  size: Size;
  quality: Quality;
  cursor: boolean;
  mic: boolean;
  micDevice: string | null;
  system: boolean;
  systemDevice: string | null;
  folder: string | null;
  service: StreamService["id"];
  server: string;
  kbps: number;
  alsoRecord: boolean;
  hide: boolean;
  countdown: boolean;
  rememberKey: boolean;
}

const DEFAULTS: Kept = {
  mode: "record",
  kind: "screen",
  fps: 30,
  size: "1080",
  quality: "standard",
  cursor: true,
  mic: true,
  micDevice: null,
  system: true,
  systemDevice: null,
  folder: null,
  service: "twitch",
  server: "",
  kbps: 6000,
  alsoRecord: false,
  hide: true,
  countdown: true,
  rememberKey: true,
};

const STORE = "nook.capture.form";

function load(): Kept {
  try {
    const raw = window.localStorage.getItem(STORE);
    return raw ? { ...DEFAULTS, ...(JSON.parse(raw) as Partial<Kept>) } : DEFAULTS;
  } catch {
    return DEFAULTS;
  }
}

function save(k: Kept) {
  try {
    window.localStorage.setItem(STORE, JSON.stringify(k));
  } catch {
    // A browser without storage only forgets the form.
  }
}

/** The source chosen last in this run, kept while the page is away. */
const chosen = { screen: null as number | null, window: null as number | null, area: null as Source | null };

export function ScreenFlow({
  say,
  preset,
}: {
  say: (message: string) => void;
  /** A request from the finder ("stream to Twitch") opens it on streaming. */
  preset: { mode: string; nonce: number } | null;
}) {
  const [form, setFormState] = useState<Kept>(load);
  const [sources, setSources] = useState<Sources | null>(null);
  const [screen, setScreen] = useState<number | null>(chosen.screen);
  const [windowHandle, setWindowHandle] = useState<number | null>(chosen.window);
  const [area, setArea] = useState<Source | null>(chosen.area);
  const [picking, setPicking] = useState(false);
  const [preview, setPreview] = useState<{ url: string | null; error: string | null; loading: boolean }>({
    url: null,
    error: null,
    loading: false,
  });
  const [previewTick, setPreviewTick] = useState(0);
  const [levels, setLevels] = useState<number[]>([]);
  const [state, setState] = useState<CaptureState>(IDLE);
  const [install, setInstall] = useState<Install | null>(null);
  const [key, setKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [counting, setCounting] = useState<number | null>(null);
  const [starting, setStarting] = useState(false);
  const sayRef = useRef(say);
  sayRef.current = say;
  const fail = useCallback((e: unknown) => sayRef.current(messageOf(e)), []);

  const set = useCallback((patch: Partial<Kept>) => {
    setFormState((f) => {
      const next = { ...f, ...patch };
      save(next);
      return next;
    });
  }, []);

  const appliedPreset = useRef<number | null>(null);
  useEffect(() => {
    if (preset && appliedPreset.current !== preset.nonce) {
      appliedPreset.current = preset.nonce;
      if (preset.mode === "stream") set({ mode: "stream" });
    }
  }, [preset, set]);

  // ---------------------------------------------------------------- what there is

  const readSources = useCallback(() => {
    captureSources().then((s) => {
      setSources(s);
      setScreen((cur) => {
        const kept = cur != null && s.screens.some((x) => x.handle === cur) ? cur : (s.screens.find((x) => x.primary)?.handle ?? null);
        chosen.screen = kept;
        return kept;
      });
      setWindowHandle((cur) => {
        const kept = cur != null && s.windows.some((x) => x.handle === cur) ? cur : null;
        chosen.window = kept;
        return kept;
      });
    }, fail);
  }, [fail]);

  useEffect(() => readSources(), [readSources]);

  useEffect(() => {
    let alive = true;
    captureState().then((s) => alive && setState(s), () => undefined);
    captureInstallState().then((i) => alive && setInstall(i), () => undefined);
    const off = onCapture((e) => {
      if (e.state) setState(e.state);
      if (e.levels) setLevels(e.levels);
      if (e.install !== undefined) {
        setInstall(e.install);
        if (e.install === null) readSources();
      }
      if (e.area !== undefined) {
        setPicking(false);
        if (e.area) {
          chosen.area = e.area;
          setArea(e.area);
          set({ kind: "area" });
        }
      }
    });
    return () => {
      alive = false;
      off();
    };
  }, [readSources, set]);

  // The stream key kept for the service, when there is one.
  useEffect(() => {
    let alive = true;
    captureStreamKey(form.service).then((k) => alive && setKey(k ?? ""), () => undefined);
    return () => {
      alive = false;
    };
  }, [form.service]);

  const source: Source | null = useMemo(() => {
    if (form.kind === "screen") return screen != null ? { kind: "screen", handle: screen } : null;
    if (form.kind === "window") return windowHandle != null ? { kind: "window", handle: windowHandle } : null;
    return area;
  }, [form.kind, screen, windowHandle, area]);

  // A picture of what it records, once FFmpeg is in.
  const sourceKey = source ? JSON.stringify(source) : "";
  useEffect(() => {
    if (!source || !sources?.ready || state.phase !== "idle") {
      setPreview({ url: null, error: null, loading: false });
      return;
    }
    let alive = true;
    setPreview((p) => ({ ...p, loading: true, error: null }));
    capturePreview(source).then(
      (url) => alive && setPreview({ url, error: null, loading: false }),
      (e) => alive && setPreview({ url: null, error: messageOf(e), loading: false }),
    );
    return () => {
      alive = false;
    };
    // `source` is read through its key, so the same choice does not ask again.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sourceKey, sources?.ready, previewTick, state.phase]);

  // The sound: what is chosen, and its meters while nothing records.
  const audio: AudioSource[] = useMemo(() => {
    const a: AudioSource[] = [];
    if (form.mic) a.push({ kind: "microphone", device: form.micDevice });
    if (form.system) a.push({ kind: "system", device: form.systemDevice });
    return a;
  }, [form.mic, form.micDevice, form.system, form.systemDevice]);
  const audioKey = JSON.stringify(audio);
  const idle = state.phase === "idle";
  useEffect(() => {
    if (!idle) return;
    setLevels([]);
    captureListen(audio).catch(fail);
    return () => {
      captureStopListening().catch(() => undefined);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [audioKey, idle, fail]);
  const micLevel = form.mic ? (levels[0] ?? 0) : 0;
  const systemLevel = form.system ? (levels[form.mic ? 1 : 0] ?? 0) : 0;

  // ---------------------------------------------------------------- starting

  const service = SERVICES.find((s) => s.id === form.service) ?? SERVICES[0];
  const server = service.server || form.server;
  const streaming = form.mode === "stream";
  const record = !streaming || form.alsoRecord;
  const streamProblem = !streaming
    ? null
    : !server.trim()
      ? "Paste the service's server address."
      : !/^(rtmps?|srt):\/\//i.test(server.trim())
        ? "The server address starts with rtmp://, rtmps:// or srt://."
        : !key.trim() && !/\/[^/]{12,}$/.test(server.trim())
          ? "Paste your stream key."
          : null;
  const ready = !!sources?.ready && source != null && !streamProblem && idle && !starting && counting == null;

  const go = async () => {
    if (!source) return;
    setStarting(true);
    try {
      if (streaming && form.rememberKey) await captureKeepStreamKey(form.service, key.trim() || null);
      if (streaming && !form.rememberKey) await captureKeepStreamKey(form.service, null);
      await captureStart(
        {
          source,
          fps: form.fps,
          scaleTo: form.size === "source" ? null : Number(form.size),
          quality: form.quality,
          cursor: form.cursor,
          audio,
          record,
          folder: form.folder,
          stream: streaming ? { server: server.trim(), key: key.trim(), kbps: Math.min(form.kbps, service.maxKbps) } : null,
        },
        form.hide,
      );
    } catch (e) {
      fail(e);
    } finally {
      setStarting(false);
    }
  };

  const start = () => {
    if (!form.countdown) {
      void go();
      return;
    }
    let n = 3;
    setCounting(n);
    const tick = window.setInterval(() => {
      n -= 1;
      if (n <= 0) {
        window.clearInterval(tick);
        setCounting(null);
        void go();
      } else setCounting(n);
    }, 1000);
  };

  const pickArea = () => {
    setPicking(true);
    capturePickArea().catch((e) => {
      setPicking(false);
      fail(e);
    });
  };

  const selectedScreen = sources?.screens.find((s) => s.handle === screen) ?? null;

  return (
    <div className="fl-flow">
      <div className="fl-column">
        <div className="fl-head">
          <div className="h5">Record your screen</div>
          <div className="body2 text-secondary">
            Record a screen, one window or an area you drag out, with your microphone and your computer's sound, or stream it live
            to Twitch, YouTube or any RTMP service. It uses your graphics card to encode when it can, and everything stays on this
            computer until you share it.
          </div>
        </div>

        {state.phase !== "idle" ? (
          <LiveCard state={state} service={streaming ? service.name : null} onError={fail} />
        ) : (
          <div className="fl-card fl-form">
            <div className="sr-top">
              <div className="nk-kind-switch" role="tablist" aria-label="Record or stream">
                <ModePill text="Record" icon="video" selected={!streaming} onClick={() => set({ mode: "record" })} />
                <ModePill text="Stream live" icon="broadcast" selected={streaming} onClick={() => set({ mode: "stream" })} />
              </div>
            </div>

            <section className="sr-section">
              <span className="overline text-tertiary">What to record</span>
              <div className="nk-kind-switch sr-kinds" role="tablist" aria-label="What to record">
                <ModePill text="A screen" icon="screen" selected={form.kind === "screen"} onClick={() => set({ kind: "screen" })} />
                <ModePill
                  text="A window"
                  icon="window"
                  selected={form.kind === "window"}
                  onClick={() => {
                    set({ kind: "window" });
                    readSources();
                  }}
                />
                <ModePill text="An area" icon="crop" selected={form.kind === "area"} onClick={() => set({ kind: "area" })} />
              </div>

              {form.kind === "screen" && (
                <div className="sr-screens" role="radiogroup" aria-label="Screens">
                  {(sources?.screens ?? []).map((s) => (
                    <button
                      key={s.handle}
                      type="button"
                      role="radio"
                      aria-checked={s.handle === screen}
                      className={s.handle === screen ? "sr-screen sr-screen--selected" : "sr-screen"}
                      onClick={() => {
                        chosen.screen = s.handle;
                        setScreen(s.handle);
                      }}
                    >
                      <span className="sr-screen__shape" style={{ aspectRatio: `${s.width} / ${s.height}` }}>
                        <Icon name="screen" size={18} />
                      </span>
                      <span className="body2">{s.name}</span>
                      <span className="caption text-tertiary">
                        {s.width} × {s.height}
                        {s.primary ? " · main" : ""}
                      </span>
                    </button>
                  ))}
                  {sources && sources.screens.length === 0 && <span className="caption text-tertiary">No screen found.</span>}
                </div>
              )}

              {form.kind === "window" && (
                <div className="sr-windows" role="radiogroup" aria-label="Windows">
                  {(sources?.windows ?? []).map((w) => (
                    <button
                      key={w.handle}
                      type="button"
                      role="radio"
                      aria-checked={w.handle === windowHandle}
                      className={w.handle === windowHandle ? "sr-window sr-window--selected" : "sr-window"}
                      onClick={() => {
                        chosen.window = w.handle;
                        setWindowHandle(w.handle);
                      }}
                    >
                      <Icon name="window" size={16} />
                      <span className="sr-window__text">
                        <span className="body2 nc-ellipsis">{w.title}</span>
                        <span className="caption text-tertiary nc-ellipsis">
                          {w.app || "An app"} · {w.width} × {w.height}
                        </span>
                      </span>
                    </button>
                  ))}
                  {sources && sources.windows.length === 0 && (
                    <span className="caption text-tertiary">No window to record: open the app you want to record, then refresh.</span>
                  )}
                  <div className="sr-windows__more">
                    <QuietAction text="Refresh the list" icon="refresh" onClick={readSources} />
                  </div>
                </div>
              )}

              {form.kind === "area" && (
                <div className="sr-area">
                  {area ? (
                    <span className="body2">{sourceText(area, sources)}</span>
                  ) : (
                    <span className="body2 text-secondary">Drag over the part of the screen you want to record.</span>
                  )}
                  <span className="nc-flex-spacer" />
                  {picking ? (
                    <>
                      <Spinner size={14} stroke={2} />
                      <span className="caption text-tertiary">Drag over your screen, then press Enter</span>
                      <TextLink text="Cancel" onClick={() => captureAreaPicked(null).catch(fail)} />
                    </>
                  ) : (
                    <Button
                      text={area ? "Choose again" : "Choose an area"}
                      icon="crop"
                      iconPosition="start"
                      variant="secondary"
                      compact
                      onClick={pickArea}
                    />
                  )}
                </div>
              )}

              {sources?.ready && source && (
                <div className="sr-preview">
                  {preview.url ? (
                    <img src={preview.url} alt={`What it records: ${sourceText(source, sources)}`} />
                  ) : (
                    <div className="sr-preview__empty">
                      {preview.loading ? <Spinner size={18} stroke={2} /> : <Icon name="screen" size={22} color="var(--text-tertiary)" />}
                      <span className="caption text-tertiary">{preview.loading ? "Taking a look…" : (preview.error ?? "")}</span>
                    </div>
                  )}
                  <IconButton
                    icon="refresh"
                    size={28}
                    iconSize={14}
                    title="Look again"
                    className="sr-preview__again"
                    onClick={() => setPreviewTick((t) => t + 1)}
                  />
                </div>
              )}
            </section>

            <section className="sr-section">
              <span className="overline text-tertiary">Sound</span>
              <SoundRow
                icon="mic"
                label="Microphone"
                on={form.mic}
                onToggle={(mic) => set({ mic })}
                devices={sources?.microphones ?? []}
                device={form.micDevice}
                onDevice={(micDevice) => set({ micDevice })}
                level={micLevel}
              />
              <SoundRow
                icon="volume"
                label="Computer sound"
                on={form.system}
                onToggle={(system) => set({ system })}
                devices={sources?.speakers ?? []}
                device={form.systemDevice}
                onDevice={(systemDevice) => set({ systemDevice })}
                level={systemLevel}
              />
            </section>

            <section className="sr-section">
              <span className="overline text-tertiary">Picture</span>
              <div className="sr-settings">
                <label className="sr-setting">
                  <span className="caption text-tertiary">Frames a second</span>
                  <div className="nk-kind-switch sr-small" role="tablist" aria-label="Frames a second">
                    <ModePill text="30" selected={form.fps === 30} onClick={() => set({ fps: 30 })} />
                    <ModePill text="60" selected={form.fps === 60} onClick={() => set({ fps: 60 })} />
                  </div>
                </label>
                <label className="sr-setting">
                  <span className="caption text-tertiary">Size</span>
                  <Dropdown
                    ariaLabel="Size"
                    minWidth={150}
                    maxWidth={200}
                    value={form.size}
                    onChange={(size) => set({ size: size as Size })}
                    options={[
                      { value: "source", label: sizeLabel(source, sources, selectedScreen) },
                      { value: "1080", label: "1080p" },
                      { value: "720", label: "720p (smaller)" },
                    ]}
                  />
                </label>
                {!streaming && (
                  <label className="sr-setting">
                    <span className="caption text-tertiary">Quality</span>
                    <div className="nk-kind-switch sr-small" role="tablist" aria-label="Quality">
                      <ModePill text="Standard" selected={form.quality === "standard"} onClick={() => set({ quality: "standard" })} />
                      <ModePill text="High" selected={form.quality === "high"} onClick={() => set({ quality: "high" })} />
                    </div>
                  </label>
                )}
                <div className="sr-setting sr-setting--toggle">
                  <Toggle checked={form.cursor} onChange={(cursor) => set({ cursor })} label="Show the mouse pointer" />
                  <span className="body2">Show the mouse pointer</span>
                </div>
              </div>
            </section>

            {streaming && (
              <section className="sr-section">
                <span className="overline text-tertiary">Stream to</span>
                <div className="sr-settings">
                  <label className="sr-setting">
                    <span className="caption text-tertiary">Service</span>
                    <Dropdown
                      ariaLabel="Streaming service"
                      minWidth={150}
                      maxWidth={200}
                      value={form.service}
                      onChange={(id) => {
                        const s = SERVICES.find((x) => x.id === id) ?? SERVICES[0];
                        set({ service: s.id, kbps: Math.min(form.kbps, s.maxKbps) });
                      }}
                      options={SERVICES.map((s) => ({ value: s.id, label: s.name }))}
                    />
                  </label>
                  <label className="sr-setting">
                    <span className="caption text-tertiary">Bit rate (kbit/s)</span>
                    <TextField
                      className="sr-kbps"
                      inputMode="numeric"
                      value={String(form.kbps)}
                      onChange={(v) => {
                        const n = Number(v.replace(/\D/g, ""));
                        if (!Number.isNaN(n)) set({ kbps: Math.min(n, service.maxKbps) });
                      }}
                    />
                  </label>
                </div>
                {!service.server && (
                  <label className="sr-field">
                    <span className="caption text-tertiary">Server address</span>
                    <TextField
                      value={form.server}
                      placeholder="rtmp://… or rtmps://…"
                      spellCheck={false}
                      onChange={(v) => set({ server: v })}
                    />
                  </label>
                )}
                <label className="sr-field">
                  <span className="caption text-tertiary">Stream key</span>
                  <div className="sr-key">
                    <TextField
                      type={showKey ? "text" : "password"}
                      value={key}
                      placeholder="Paste your stream key"
                      autoComplete="off"
                      spellCheck={false}
                      onChange={setKey}
                    />
                    <IconButton
                      icon={showKey ? "eye-off" : "eye"}
                      size={32}
                      iconSize={16}
                      title={showKey ? "Hide the key" : "Show the key"}
                      onClick={() => setShowKey((s) => !s)}
                    />
                  </div>
                  <span className="caption text-tertiary">{service.keyHelp}. Keep it private: anyone with it can stream as you.</span>
                </label>
                <div className="sr-settings">
                  <div className="sr-setting sr-setting--toggle">
                    <Toggle checked={form.rememberKey} onChange={(rememberKey) => set({ rememberKey })} label="Remember the key" />
                    <span className="body2">
                      Remember the key on this computer ({isMac ? "kept in your Mac's keychain" : "encrypted for your Windows account"})
                    </span>
                  </div>
                  <div className="sr-setting sr-setting--toggle">
                    <Toggle checked={form.alsoRecord} onChange={(alsoRecord) => set({ alsoRecord })} label="Also save a recording" />
                    <span className="body2">Also save a recording</span>
                  </div>
                </div>
              </section>
            )}

            <section className="sr-section">
              {record && (
                <div className="sr-folder">
                  <span className="caption text-tertiary">Save to</span>
                  <span className="caption text-secondary nc-ellipsis">{form.folder ?? sources?.folder ?? "Videos › Nook"}</span>
                  <TextLink text="Change" onClick={() => chooseRecordingFolder().then((f) => f && set({ folder: f }), fail)} />
                  <TextLink text="Open" onClick={() => captureOpenFolder(form.folder).catch(fail)} />
                  {form.folder && <TextLink text="Videos › Nook" onClick={() => set({ folder: null })} />}
                </div>
              )}
              <div className="sr-settings">
                <div className="sr-setting sr-setting--toggle">
                  <Toggle checked={form.hide} onChange={(hide) => set({ hide })} label="Hide Nook while recording" />
                  <span className="body2">Hide Nook while recording</span>
                </div>
                <div className="sr-setting sr-setting--toggle">
                  <Toggle checked={form.countdown} onChange={(countdown) => set({ countdown })} label="Count down from 3" />
                  <span className="body2">Count down from 3</span>
                </div>
              </div>
            </section>

            {sources && !sources.ready && (
              <DownloadLine
                text="One-time download: FFmpeg, which records and streams"
                bytes={sources.downloadBytes}
                install={install}
                onDownload={() => captureInstall().catch(fail)}
                onStop={() => captureCancelInstall().catch(fail)}
                onRetry={() => captureClearInstallError().then(() => captureInstall(), fail)}
              />
            )}

            <div className="fl-form__go">
              <span className="caption text-tertiary">
                {streamProblem ?? (source ? goText(form, source, sources, record) : "Choose what to record.")}
              </span>
              <Button
                text={counting != null ? `Starting in ${counting}…` : streaming ? "Go live" : "Start recording"}
                icon={streaming ? "broadcast" : "record"}
                iconPosition="start"
                disabled={!ready}
                onClick={start}
              />
            </div>
          </div>
        )}

        {idle && state.error && (
          <div className="fl-card sr-note">
            <Icon name="alert-circle" size={16} color="var(--warning)" />
            <span className="body2">{state.error}</span>
          </div>
        )}
        {idle && state.saved && <SavedCard saved={state.saved} onError={fail} />}
      </div>
    </div>
  );
}

function sizeLabel(
  source: Source | null,
  sources: Sources | null,
  screen: { width: number; height: number } | null,
): string {
  if (source?.kind === "area") return `Its own (${source.width} × ${source.height})`;
  if (source?.kind === "window") {
    const w = sources?.windows.find((x) => x.handle === source.handle);
    return w ? `Its own (${w.width} × ${w.height})` : "Its own";
  }
  return screen ? `Its own (${screen.width} × ${screen.height})` : "Its own";
}

function goText(form: Kept, source: Source, sources: Sources | null, record: boolean): string {
  const what = sourceText(source, sources);
  const sound =
    form.mic && form.system ? "with your microphone and the computer's sound" : form.mic ? "with your microphone" : form.system ? "with the computer's sound" : "without sound";
  if (form.mode === "stream") {
    const s = SERVICES.find((x) => x.id === form.service)?.name ?? "the service";
    return `${what}, ${sound}, live on ${s}${record ? ", and saved" : ""}.`;
  }
  return `${what}, ${sound}, at ${form.fps} frames a second.`;
}

function SoundRow({
  icon,
  label,
  on,
  onToggle,
  devices,
  device,
  onDevice,
  level,
}: {
  icon: string;
  label: string;
  on: boolean;
  onToggle: (on: boolean) => void;
  devices: { name: string; default: boolean }[];
  device: string | null;
  onDevice: (device: string | null) => void;
  level: number;
}) {
  const options = [
    { value: "", label: `${isMac ? "The Mac's" : "Windows'"} default${devices.find((d) => d.default) ? ` (${devices.find((d) => d.default)!.name})` : ""}` },
    ...devices.map((d) => ({ value: d.name, label: d.name })),
  ];
  const known = device == null || devices.length === 0 || devices.some((d) => d.name === device);
  return (
    <div className={on ? "sr-sound" : "sr-sound sr-sound--off"}>
      <Toggle checked={on} onChange={onToggle} label={label} />
      <Icon name={icon} size={16} color={on ? "var(--text-secondary)" : "var(--text-disabled)"} />
      <span className="body2 sr-sound__label">{label}</span>
      <Dropdown
        ariaLabel={`${label}: device`}
        minWidth={180}
        maxWidth={300}
        disabled={!on}
        value={known ? (device ?? "") : ""}
        onChange={(v) => onDevice(v || null)}
        options={options}
      />
      <span className="sr-meter" aria-hidden="true">
        <span className="sr-meter__fill" style={{ width: `${Math.round(Math.min(1, Math.sqrt(level)) * 100)}%` }} />
      </span>
    </div>
  );
}

function LiveCard({ state, service, onError }: { state: CaptureState; service: string | null; onError: (e: unknown) => void }) {
  const paused = state.phase === "paused";
  const title =
    state.phase === "starting"
      ? "Starting…"
      : state.phase === "finishing"
        ? state.recording
          ? "Saving the recording…"
          : "Ending the stream…"
        : paused
          ? "Paused"
          : state.streaming
            ? `Live${service ? ` on ${service}` : ""}${state.recording ? ", and recording" : ""}`
            : "Recording";
  const busy = state.phase === "starting" || state.phase === "finishing";
  return (
    <div className="fl-card sr-live">
      <div className="sr-live__head">
        <span className={paused || busy ? "sr-dot sr-dot--still" : "sr-dot"} />
        <span className="subtitle1">{title}</span>
        <span className="nc-flex-spacer" />
        <span className="h5 sr-live__clock">{clock(state.seconds)}</span>
      </div>
      <div className="sr-live__facts caption text-tertiary">
        {state.width > 0 && (
          <span>
            {state.width} × {state.height}
          </span>
        )}
        {state.fps > 0 && <span>{state.fps.toFixed(0)} frames a second</span>}
        {state.kbps > 0 && <span>{(state.kbps / 1000).toFixed(1)} Mbit/s</span>}
        {state.bytes > 0 && state.recording && <span>{bytesText(state.bytes)}</span>}
        {state.dropped > 0 && <span className="text-warning">{state.dropped} frames dropped</span>}
        {state.encoder && <span>Encoded by the {state.encoder}</span>}
      </div>
      {state.streamError && (
        <div className="fl-form__problem">
          <Icon name="alert-circle" size={14} color="var(--warning)" />
          <span className="caption text-warning">{state.streamError}</span>
        </div>
      )}
      <div className="sr-live__actions">
        {busy && <Spinner size={16} stroke={2} />}
        {!busy && !state.streaming && (
          <Button
            text={paused ? "Resume" : "Pause"}
            icon={paused ? "record" : "pause"}
            iconPosition="start"
            variant="secondary"
            onClick={() => (paused ? captureResume() : capturePause()).catch(onError)}
          />
        )}
        {!busy && (
          <Button text={state.streaming ? "End the stream" : "Stop and save"} icon="stop" iconPosition="start" onClick={() => captureStop().catch(onError)} />
        )}
      </div>
    </div>
  );
}

function SavedCard({ saved, onError }: { saved: { path: string; bytes: number; seconds: number }; onError: (e: unknown) => void }) {
  const name = saved.path.split(/[\\/]/).pop() ?? saved.path;
  return (
    <div className="fl-card fl-run">
      <div className="fl-run__head">
        <span className="fl-run__icon">
          <Icon name="check" size={16} color="var(--success)" />
        </span>
        <span className="subtitle2 fl-run__name nc-ellipsis">{name}</span>
      </div>
      <span className="caption text-tertiary">
        {clock(saved.seconds)} · {bytesText(saved.bytes)} · MP4
      </span>
      <div className="sr-saved__actions">
        <QuietAction text="Open" icon="launch" onClick={() => captureOpen(saved.path).catch(onError)} />
        <QuietAction text="Show in folder" icon="folder-open" onClick={() => captureReveal(saved.path).catch(onError)} />
      </div>
    </div>
  );
}
