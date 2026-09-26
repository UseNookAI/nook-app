/**
 * What the Models page reads, kept current while it is open: the catalog, the installed models and
 * ModelDownloadService's downloads (SettingsViewModel's pass-throughs), and RuntimeManager's own
 * downloads (polled every second, as ModelDownloadsStrip and HubBrowserView did). "downloads" events
 * re-read the downloads at once; the installed list is read again whenever the set in flight changes
 * (a download ended, a model was deleted), as refreshSync() ran after each.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import {
  EMPTY_DOWNLOADS,
  modelsCatalog,
  modelsDownloads,
  modelsInstalled,
  onDownloads,
  type Catalog,
  type DownloadsState,
  type LocalModel,
} from "../../../api/models";
import { runtimeDownloads, runtimeRefreshModels } from "../../../api/runtime";

export interface ModelsData {
  /** Null until read (the page shows its spinner); an empty catalog when it could not be read. */
  catalog: Catalog | null;
  installed: LocalModel[];
  downloads: DownloadsState;
  /** RuntimeManager.downloads(): hub variants and agent downloads, by key. */
  runtimeDownloads: Record<string, number>;
  /** Reads the downloads and the installed models again, after an action. */
  reload: () => void;
}

const EMPTY_CATALOG: Catalog = {
  models: [],
  defaultChatModel: null,
  defaultWorkerModel: null,
  defaultSpeechModel: null,
  defaultImageModel: null,
  defaultVideoModel: null,
};

const sameMap = (a: Record<string, number>, b: Record<string, number>) => {
  const ka = Object.keys(a);
  return ka.length === Object.keys(b).length && ka.every((k) => a[k] === b[k]);
};

export function useModelsData(): ModelsData {
  const [catalog, setCatalog] = useState<Catalog | null>(null);
  const [installed, setInstalled] = useState<LocalModel[]>([]);
  const [downloads, setDownloads] = useState<DownloadsState>(EMPTY_DOWNLOADS);
  const [runtime, setRuntime] = useState<Record<string, number>>({});
  const reloadRef = useRef<() => void>(() => {});

  useEffect(() => {
    let alive = true;
    // The keys seen last; null before the first read (the installed list is read on open anyway).
    let inFlight: string | null = null;
    let runtimeKeys: string | null = null;
    const readInstalled = () =>
      modelsInstalled()
        .then((list) => alive && setInstalled(list))
        .catch(() => undefined);
    const readDownloads = () =>
      modelsDownloads()
        .then((d) => {
          if (!alive) return;
          setDownloads(d);
          const keys = [...d.downloadingModels, ...d.pausedModels, ...d.stoppingModels, ...d.deletingModels].sort().join("\n");
          if (inFlight !== null && keys !== inFlight) readInstalled();
          inFlight = keys;
        })
        .catch(() => undefined);
    const readRuntime = () =>
      runtimeDownloads()
        .then((r) => {
          if (!alive) return;
          setRuntime((prev) => (sameMap(prev, r) ? prev : r));
          // A hub download that ended is an installed model now.
          const keys = Object.keys(r).sort().join("\n");
          if (runtimeKeys !== null && keys !== runtimeKeys) readInstalled();
          runtimeKeys = keys;
        })
        .catch(() => undefined);

    modelsCatalog()
      .then((c) => alive && setCatalog(c))
      .catch(() => alive && setCatalog(EMPTY_CATALOG));
    readInstalled();
    readDownloads();
    readRuntime();
    // LaunchedEffect(Unit) { downloadService.refresh() }: the models folder is scanned afresh on open.
    runtimeRefreshModels()
      .catch(() => undefined)
      .finally(() => {
        if (alive) readInstalled();
      });

    reloadRef.current = () => {
      readDownloads();
      readInstalled();
      readRuntime();
    };
    const off = onDownloads(() => {
      readDownloads();
      readRuntime();
    });
    const poll = window.setInterval(readRuntime, 1000);
    return () => {
      alive = false;
      off();
      window.clearInterval(poll);
    };
  }, []);

  const reload = useCallback(() => reloadRef.current(), []);
  return { catalog, installed, downloads, runtimeDownloads: runtime, reload };
}
