/**
 * The document converter Nooklet: files dropped or chosen (several at once, of any kinds Nook
 * reads), what they can all become in the picker's groups with who does the work, anything still
 * to download as one button with its size, and where the results go (beside each file, or a
 * folder). The conversions follow, newest first, each file with its results to open or show.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import {
  chooseDocuments,
  chooseOutputFolder,
  convertCancel,
  convertCancelInstall,
  convertClearInstallError,
  convertInstall,
  convertInstallState,
  convertJobs,
  convertOffer,
  convertOpen,
  convertReveal,
  convertStart,
  onConvert,
  type Job,
  type Offer,
  type Target,
} from "../../api/convert";
import type { Install } from "../../api/flows";
import { messageOf } from "../../api/ipc";
import { QuietAction, TextLink } from "../../components/Activity";
import { Button, IconButton } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { Spinner } from "../../components/Spinner";
import { Toggle } from "../../components/Toggle";
import {
  fileName,
  goText,
  groupTargets,
  jobStatusText,
  jobTitle,
  kindIcon,
  needsBytes,
  needsText,
  pickTarget,
} from "./convertFormat";
import { DownloadLine } from "./DownloadLine";
import { splitPath } from "./format";
import { useFilesDrop } from "./useFileDrop";

/** What the form keeps while the person is on another page. */
const kept = { files: [] as string[], target: null as string | null, folder: null as string | null, combine: true };

const newestFirst = (a: Job, b: Job) => b.at - a.at || (b.id < a.id ? -1 : 1);

export function ConvertFlow({
  say,
  preset,
}: {
  say: (message: string) => void;
  /** A request from the finder ("to PDF") chooses the format. */
  preset: { format: string; label: string; nonce: number } | null;
}) {
  const [files, setFilesState] = useState<string[]>(kept.files);
  const [chosen, setChosenState] = useState<string | null>(kept.target);
  const [folder, setFolderState] = useState<string | null>(kept.folder);
  const [combine, setCombineState] = useState(kept.combine);
  const [asked, setAsked] = useState<{ format: string; label: string } | null>(null);
  const [offer, setOffer] = useState<Offer | null>(null);
  const [install, setInstall] = useState<Install | null>(null);
  const [jobs, setJobs] = useState<Job[]>([]);
  const [starting, setStarting] = useState(false);
  /** Bumped when what is installed may have changed, so the offer is read again. */
  const [offerTick, setOfferTick] = useState(0);
  const sayRef = useRef(say);
  sayRef.current = say;
  const fail = useCallback((e: unknown) => sayRef.current(messageOf(e)), []);

  const setFiles = (next: string[]) => {
    kept.files = next;
    setFilesState(next);
  };
  const setChosen = (next: string | null) => {
    kept.target = next;
    setChosenState(next);
  };
  const setFolder = (next: string | null) => {
    kept.folder = next;
    setFolderState(next);
  };
  const setCombine = (next: boolean) => {
    kept.combine = next;
    setCombineState(next);
  };

  // A request from the finder ("to PDF") chooses the format once the files can become it.
  const appliedPreset = useRef<number | null>(null);
  useEffect(() => {
    if (preset && appliedPreset.current !== preset.nonce) {
      appliedPreset.current = preset.nonce;
      setAsked({ format: preset.format, label: preset.label });
      kept.target = null;
      setChosenState(null);
    }
  }, [preset]);

  // ---------------------------------------------------------------- reading

  useEffect(() => {
    let alive = true;
    if (files.length === 0) {
      setOffer(null);
      return;
    }
    convertOffer(files).then((o) => alive && setOffer(o), fail);
    return () => {
      alive = false;
    };
  }, [files, offerTick, fail]);

  useEffect(() => {
    let alive = true;
    convertJobs().then((j) => alive && setJobs([...j].sort(newestFirst)), fail);
    convertInstallState().then((i) => alive && setInstall(i), () => undefined);
    const off = onConvert((e) => {
      if (e.job) {
        const job = e.job;
        setJobs((all) => [job, ...all.filter((j) => j.id !== job.id)].sort(newestFirst));
      }
      if (e.install !== undefined) {
        setInstall(e.install);
        if (e.install == null) setOfferTick((t) => t + 1);
      }
    });
    return () => {
      alive = false;
      off();
    };
  }, [fail]);

  // ---------------------------------------------------------------- files

  const addFiles = (paths: string[]) => {
    const next = [...files];
    for (const p of paths) if (!next.includes(p)) next.push(p);
    setFiles(next);
  };
  const { hover } = useFilesDrop(true, addFiles);
  const choose = () => chooseDocuments().then((picked) => picked.length > 0 && addFiles(picked), fail);

  // ---------------------------------------------------------------- the target

  const targetId = pickTarget(offer, chosen, asked?.format ?? null);
  const target: Target | null = offer?.targets.find((t) => t.id === targetId) ?? null;
  const known = offer?.files.filter((f) => f.format) ?? [];
  const combining = combine && !!offer?.combine && target?.id === "pdf";
  const ready = target != null && target.needs.length === 0 && !target.missing && known.length > 0 && !starting;
  const askedName = asked && offer && !target ? asked.label : null;

  const download = () => target && convertInstall(target.needs.map((n) => n.engine)).catch(fail);

  const start = () => {
    if (!target || !offer) return;
    setStarting(true);
    convertStart(
      known.map((f) => f.path),
      target.id,
      combining,
      folder,
    )
      .then((job) => {
        setJobs((all) => [job, ...all.filter((j) => j.id !== job.id)].sort(newestFirst));
        setFiles([]);
        setAsked(null);
      }, fail)
      .finally(() => setStarting(false));
  };

  return (
    <div className="fl-flow">
      <div className="fl-column">
        <div className="fl-head">
          <div className="h5">Convert documents</div>
          <div className="body2 text-secondary">
            Drop in documents, sheets, slides, e-books or pictures and pick what they should become: Word to PDF, PDF to Word,
            Excel to CSV, Markdown to a web page, photos into one PDF, and more. Office files keep their layout. Everything runs on
            this computer.
          </div>
        </div>

        <div className="fl-card fl-form">
          {files.length === 0 ? (
            <div
              className={hover ? "fl-drop fl-drop--hover" : "fl-drop"}
              role="button"
              tabIndex={0}
              onClick={choose}
              onKeyDown={(e) => e.key === "Enter" && choose()}
            >
              <Icon name="upload" size={24} color={hover ? "var(--primary-variant)" : "var(--text-secondary)"} />
              <div className="subtitle2">{hover ? "Drop them to convert them" : "Drop files here"}</div>
              <div className="caption text-tertiary">or click to choose · Word, Excel, PowerPoint, PDF, Markdown, e-books, pictures and more</div>
            </div>
          ) : (
            <div className={hover ? "cv-files cv-files--hover" : "cv-files"}>
              {(offer?.files ?? files.map((p) => ({ path: p, name: splitPath(p).name, format: null, formatName: null, kind: null }))).map(
                (f) => (
                  <div className="cv-file" key={f.path}>
                    <span className={f.kind || !offer ? "cv-file__icon" : "cv-file__icon cv-file__icon--unknown"}>
                      <Icon name={offer ? kindIcon(f.kind) : "text"} size={16} />
                    </span>
                    <span className="cv-file__text">
                      <span className="body2 nc-ellipsis">{f.name}</span>
                      <span className={f.format || !offer ? "caption text-tertiary nc-ellipsis" : "caption text-warning nc-ellipsis"}>
                        {offer ? (f.formatName ?? "Nook does not read this kind of file") : splitPath(f.path).folder}
                      </span>
                    </span>
                    <IconButton icon="close" size={28} iconSize={14} title="Leave it out" onClick={() => setFiles(files.filter((p) => p !== f.path))} />
                  </div>
                ),
              )}
              <div className="cv-files__more">
                <QuietAction text={hover ? "Drop to add them" : "Add files"} icon="plus" onClick={choose} />
                <span className="nc-flex-spacer" />
                <TextLink text="Clear" onClick={() => setFiles([])} />
              </div>
            </div>
          )}

          {offer?.note && (
            <div className="fl-form__problem">
              <Icon name="alert-circle" size={14} color="var(--warning)" />
              <span className="caption text-warning">{offer.note}</span>
            </div>
          )}

          {offer && offer.targets.length > 0 && (
            <div className="cv-targets">
              <span className="overline text-tertiary">Convert into</span>
              {groupTargets(offer.targets).map((g) => (
                <div className="cv-group" key={g.kind}>
                  <span className="caption text-tertiary">{g.label}</span>
                  <div className="cv-group__pills" role="radiogroup" aria-label={g.label}>
                    {g.targets.map((t) => (
                      <button
                        key={t.id}
                        type="button"
                        role="radio"
                        aria-checked={t.id === targetId}
                        disabled={!!t.missing}
                        title={t.missing ?? `${t.name} · by ${t.by}${t.needs.length > 0 ? " · needs a download" : ""}`}
                        className={[
                          "cv-target",
                          t.id === targetId ? "cv-target--selected" : "",
                          t.needs.length > 0 ? "cv-target--needs" : "",
                        ]
                          .filter(Boolean)
                          .join(" ")}
                        onClick={() => setChosen(t.id)}
                      >
                        {t.name}
                      </button>
                    ))}
                  </div>
                </div>
              ))}
            </div>
          )}

          {askedName && offer && offer.targets.length > 0 && (
            <div className="caption text-tertiary">These files cannot become {askedName}; pick another format.</div>
          )}

          {offer && known.length > 0 && (
            <div className="cv-options">
              {offer.combine && target?.id === "pdf" && (
                <div className="cv-option">
                  <Toggle checked={combine} onChange={setCombine} label="Put every picture into one PDF" />
                  <span className="body2">Put every picture into one PDF, one page each</span>
                </div>
              )}
              <div className="cv-option">
                <span className="caption text-tertiary">Save to</span>
                <span className="caption text-secondary nc-ellipsis">{folder ?? "the same folder as each file"}</span>
                <TextLink text="Change" onClick={() => chooseOutputFolder().then((f) => f && setFolder(f), fail)} />
                {folder && <TextLink text="Beside each file" onClick={() => setFolder(null)} />}
              </div>
            </div>
          )}

          {target && target.needs.length > 0 && (
            <DownloadLine
              text={needsText(target.needs)}
              bytes={needsBytes(target.needs)}
              install={install}
              onDownload={download}
              onStop={() => convertCancelInstall().catch(fail)}
              onRetry={() => convertClearInstallError().then(download, fail)}
            />
          )}

          {files.length > 0 && (
            <div className="fl-form__go">
              <span className="caption text-tertiary">{offer ? goText(known, target, combining) : ""}</span>
              <Button text="Convert" icon="file-convert" iconPosition="start" disabled={!ready} onClick={start} />
            </div>
          )}
        </div>

        {jobs.length > 0 && (
          <div className="fl-runs-head">
            <span className="overline text-tertiary">Conversions</span>
          </div>
        )}
        {jobs.map((job) => (
          <JobCard key={job.id} job={job} onError={fail} />
        ))}
      </div>
    </div>
  );
}

function StatusIcon({ status }: { status: Job["status"] }) {
  switch (status) {
    case "CONVERTING":
      return <Spinner size={14} stroke={2} />;
    case "DONE":
      return <Icon name="check" size={16} color="var(--success)" />;
    case "FAILED":
      return <Icon name="alert-circle" size={16} color="var(--warning)" />;
    case "STOPPED":
      return <Icon name="stop" size={16} color="var(--text-tertiary)" />;
    default:
      return <Icon name="loading" size={16} color="var(--text-tertiary)" />;
  }
}

/** Results shown by name before "and N more". */
const SHOWN = 3;

function JobCard({ job, onError }: { job: Job; onError: (e: unknown) => void }) {
  const running = job.status === "WAITING" || job.status === "CONVERTING";
  return (
    <div className="fl-card fl-run">
      <div className="fl-run__head">
        <span className="fl-run__icon">
          <StatusIcon status={job.status} />
        </span>
        <span className="subtitle2 fl-run__name">{jobTitle(job)}</span>
        <span className="nc-flex-spacer" />
        {running && <Button text="Stop" variant="secondary" compact onClick={() => convertCancel(job.id).catch(onError)} />}
      </div>
      <span className="caption text-tertiary">{jobStatusText(job)}</span>
      {job.items.map((item) => (
        <div className="cv-item" key={item.input + item.name}>
          {job.items.length > 1 && (
            <div className="cv-item__line">
              <StatusIcon status={item.status} />
              <span className="body2 nc-ellipsis">{item.name}</span>
            </div>
          )}
          {item.outputs.length > 0 && (
            <div className="cv-item__outputs" style={job.items.length > 1 ? undefined : { paddingLeft: 0 }}>
              {item.outputs.slice(0, SHOWN).map((out) => (
                <QuietAction key={out} text={fileName(out)} icon="launch" title={`Open ${out}`} onClick={() => convertOpen(out).catch(onError)} />
              ))}
              {item.outputs.length > SHOWN && <span className="caption text-tertiary">and {item.outputs.length - SHOWN} more</span>}
              <QuietAction text="Show in folder" icon="folder-open" onClick={() => convertReveal(item.outputs[0]).catch(onError)} />
            </div>
          )}
          {item.error && (
            <span className="caption text-warning cv-item__error" style={job.items.length > 1 ? undefined : { paddingLeft: 0 }}>
              {item.error}
            </span>
          )}
        </div>
      ))}
    </div>
  );
}
