/**
 * Models › Browse (HubBrowserView.kt): every GGUF on Hugging Face, searchable from the field in the
 * Settings header, with each quantisation sized against this GPU. A download lands in the models
 * folder and shows up as an installed model like any other.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { messageOf } from "../../../api/ipc";
import { hubCancel, hubDownload, hubInstalled, hubKey, hubSearch, hubVariants, type Repo, type Variant } from "../../../api/models";
import { gpuSnapshot } from "../../../api/runtime";
import { Chip, QuietAction } from "../../../components/Activity";
import { compact } from "../../../components/activityFormat";
import { Button } from "../../../components/Button";
import { Icon } from "../../../components/Icon";
import { useSnackbar } from "../../../components/Snackbar";
import { Spinner } from "../../../components/Spinner";
import { fit, fitChip, labelShared, looksLikeEmbedding, percent, QUICK_SEARCHES, repoFacts, repoTitle, sizingNote, variantSize } from "./hub";
import "./models.css";

export function HubBrowserView({
  query: fieldQuery,
  submit,
  onQuickSearch,
  downloads,
}: {
  /** The text in the header's search field. */
  query: string;
  /** Bumped when Enter is pressed in the field: search at once. */
  submit: number;
  /** A quick search was clicked: it goes into the field and is searched at once. */
  onQuickSearch: (text: string) => void;
  /** RuntimeManager.downloads(), polled by the page. */
  downloads: Record<string, number>;
}) {
  const { say } = useSnackbar();
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<Repo[]>([]);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [searched, setSearched] = useState(false);
  const [openRepo, setOpenRepo] = useState<string | null>(null);
  const [variants, setVariants] = useState<Record<string, Variant[]>>({});
  const [variantErrors, setVariantErrors] = useState<Record<string, string>>({});
  const [loadingVariants, setLoadingVariants] = useState<string | null>(null);
  const [installed, setInstalled] = useState<Record<string, ReadonlySet<string>>>({});
  const [gpuTotal, setGpuTotal] = useState(0);
  const [installedTick, setInstalledTick] = useState(0);
  const lastQuery = useRef("");
  const searchSeq = useRef(0);

  // The largest card, for sizing every variant against.
  useEffect(() => {
    gpuSnapshot()
      .then((s) => setGpuTotal(s.devices.reduce((max, d) => Math.max(max, d.totalBytes), 0)))
      .catch(() => setGpuTotal(0));
  }, []);

  // A download that finished (the map shrank) may be an installed variant now.
  const previousDownloads = useRef(Object.keys(downloads).length);
  useEffect(() => {
    const size = Object.keys(downloads).length;
    if (size < previousDownloads.current) setInstalledTick((t) => t + 1);
    previousDownloads.current = size;
  }, [downloads]);

  const search = useCallback((text: string) => {
    const q = text.trim();
    const seq = ++searchSeq.current;
    lastQuery.current = q;
    setQuery(q);
    setSearching(true);
    setSearchError(null);
    setOpenRepo(null);
    hubSearch(q, 30)
      .then((r) => seq === searchSeq.current && setResults(r))
      .catch((e) => seq === searchSeq.current && setSearchError(messageOf(e) || "Search failed."))
      .finally(() => {
        if (seq !== searchSeq.current) return;
        setSearching(false);
        setSearched(true);
      });
  }, []);

  // Popular models come up first so the page is never empty.
  useEffect(() => search(""), [search]);
  // The header field: Enter searches at once, otherwise a pause in typing does.
  const firstSubmit = useRef(submit);
  useEffect(() => {
    if (submit > 0 && submit !== firstSubmit.current) search(fieldQuery);
    // Only a new Enter searches; the field's text is read as it is then.
  }, [submit, search]);
  useEffect(() => {
    const t = window.setTimeout(() => {
      if (fieldQuery.trim() !== lastQuery.current) search(fieldQuery);
    }, 500);
    return () => window.clearTimeout(t);
  }, [fieldQuery, search]);

  const open = (repo: Repo) => {
    if (openRepo === repo.id) {
      setOpenRepo(null);
      return;
    }
    setOpenRepo(repo.id);
    if (variants[repo.id]) return;
    setLoadingVariants(repo.id);
    hubVariants(repo.id)
      .then((v) => setVariants((m) => ({ ...m, [repo.id]: v })))
      .catch((e) => setVariantErrors((m) => ({ ...m, [repo.id]: messageOf(e) || "Could not list files." })))
      .finally(() => setLoadingVariants((id) => (id === repo.id ? null : id)));
  };

  // Which variants of the open repository are already in the models folder.
  const openVariants = openRepo ? variants[openRepo] : undefined;
  useEffect(() => {
    if (!openRepo || !openVariants || openVariants.length === 0) return;
    let alive = true;
    hubInstalled(openRepo, openVariants)
      .then((keys) => alive && setInstalled((m) => ({ ...m, [openRepo]: new Set(keys) })))
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [openRepo, openVariants, installedTick]);

  const download = (repo: Repo, v: Variant) => hubDownload(repo, v).catch((e) => say(messageOf(e)));
  const cancel = (repo: Repo, v: Variant) => hubCancel(repo.id, v.key).catch((e) => say(messageOf(e)));

  return (
    <div className="nk-models__body">
      <div className="nk-hub__quick">
        <span className="caption text-tertiary">Try</span>
        {QUICK_SEARCHES.map((q) => (
          <button key={q} type="button" className="nk-hub__quick-chip" onClick={() => onQuickSearch(q)}>
            <Chip text={q} accent={query.toLowerCase() === q.toLowerCase()} />
          </button>
        ))}
        <span className="nk-hub__quick-spacer" />
        {searching && <Spinner size={16} stroke={2} />}
      </div>

      <div className="caption nk-models__note">{sizingNote(gpuTotal)}</div>

      {searchError && <div className="body2 nk-models__error">{searchError}</div>}

      {searching && results.length === 0 ? (
        <div className="nk-hub__loading">
          <Spinner size={24} stroke={2} />
        </div>
      ) : searched && results.length === 0 && searchError == null ? (
        <div className="body2 text-secondary">Nothing with GGUF files matches “{query}”.</div>
      ) : (
        <div className="nk-models__scroll">
          <div className="nk-hub__list">
            {results.map((repo) => (
              <RepoRow
                key={repo.id}
                repo={repo}
                open={openRepo === repo.id}
                loading={loadingVariants === repo.id}
                variants={variants[repo.id]}
                error={variantErrors[repo.id]}
                installed={installed[repo.id]}
                gpuTotal={gpuTotal}
                downloads={downloads}
                onToggle={() => open(repo)}
                onDownload={(v) => download(repo, v)}
                onCancel={(v) => cancel(repo, v)}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function RepoRow({
  repo,
  open,
  loading,
  variants,
  error,
  installed,
  gpuTotal,
  downloads,
  onToggle,
  onDownload,
  onCancel,
}: {
  repo: Repo;
  open: boolean;
  loading: boolean;
  variants: Variant[] | undefined;
  error: string | undefined;
  installed: ReadonlySet<string> | undefined;
  gpuTotal: number;
  downloads: Record<string, number>;
  onToggle: () => void;
  onDownload: (v: Variant) => void;
  onCancel: (v: Variant) => void;
}) {
  return (
    <div className={open ? "nk-repo nk-repo--open" : "nk-repo"}>
      <button type="button" className="nk-repo__header" aria-expanded={open} onClick={onToggle}>
        <span className="nk-repo__text">
          <span className="nk-repo__title-row">
            <span className="subtitle2 nk-repo__title">{repoTitle(repo)}</span>
            {repo.gated && <Chip text="Gated" icon="lock" />}
            {looksLikeEmbedding(repo) && <Chip text="Embedding" />}
          </span>
          <span className="caption nk-repo__facts">{repoFacts(repo, compact)}</span>
        </span>
        <Icon name="arrow-down" size={12} className="nk-repo__chevron" />
      </button>

      {open && (
        <div className="nk-repo__body">
          {loading ? (
            <div className="nk-repo__listing">
              <Spinner size={16} stroke={2} />
              <span className="caption text-secondary">Listing files…</span>
            </div>
          ) : error != null ? (
            <div className="caption text-error">{error}</div>
          ) : !variants || variants.length === 0 ? (
            <div className="caption text-secondary">No complete GGUF files in this repository.</div>
          ) : repo.gated ? (
            <div className="caption text-secondary">
              This repository is gated: accept its licence on huggingface.co and download the file into Nook's models folder by hand.
            </div>
          ) : (
            variants.map((v) => (
              <VariantRow
                key={v.key}
                v={v}
                showFileName={labelShared(variants, v)}
                progress={downloads[hubKey(repo.id, v.key)]}
                installed={installed?.has(v.key) === true}
                gpuTotal={gpuTotal}
                onDownload={() => onDownload(v)}
                onCancel={() => onCancel(v)}
              />
            ))
          )}
        </div>
      )}
    </div>
  );
}

function VariantRow({
  v,
  showFileName,
  progress,
  installed,
  gpuTotal,
  onDownload,
  onCancel,
}: {
  v: Variant;
  showFileName: boolean;
  progress: number | undefined;
  installed: boolean;
  gpuTotal: number;
  onDownload: () => void;
  onCancel: () => void;
}) {
  const [fitText, fitAccent] = fitChip(fit(v.totalBytes, gpuTotal));
  return (
    <div className="nk-variant">
      <div className="nk-variant__row">
        <div className={showFileName ? "nk-variant__name nk-variant__name--wide" : "nk-variant__name"}>
          <div className="code nk-variant__label">{v.label}</div>
          {showFileName && <div className="caption nk-variant__file">{v.key}</div>}
        </div>
        <span className="numeric nk-variant__size">{variantSize(v)}</span>
        <Chip text={fitText} accent={fitAccent} />
        <span className="nk-variant__spacer" />
        {installed ? (
          <Chip text="Installed" accent icon="check" />
        ) : progress != null ? (
          <span className="nk-variant__progress">
            <span className="numeric text-secondary">{percent(progress)}</span>
            <QuietAction text="Cancel" onClick={onCancel} />
          </span>
        ) : (
          <Button text="Download" className="nk-variant__download" onClick={onDownload} />
        )}
      </div>
      {progress != null && !installed && (
        <div className="nk-variant__bar">
          <div className="nk-variant__bar-fill" style={{ width: `${Math.max(0, Math.min(1, progress)) * 100}%` }} />
        </div>
      )}
    </div>
  );
}
