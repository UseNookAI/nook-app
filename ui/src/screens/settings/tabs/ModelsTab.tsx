/**
 * The Models page of Settings (AiModelsSettingsView.kt with ModelsPageState, HubBrowserView.kt,
 * WorkerModelsView.kt, WebAccessSetting.kt, ModelCard.kt, ModelDownloadsStrip.kt; the pieces are in
 * ../models).
 *
 * What the frame gives it (SettingsPopup.tsx):
 * - It fills the page area under the header, full width, and scrolls itself, inside the frame's
 *   24 px sides (var(--space-xl)).
 * - `headerSlot` is the header row between the "Models" title and the close button (flex: 1, items
 *   centred, end-aligned). The downloads strip (weight 1, 16 px sides) and the 320 px search field
 *   (10 px before the close button) go there with `createPortal(..., headerSlot)`. It is null on
 *   the first render.
 * - Model downloads are reported to the quit dialog (BusyKey.DOWNLOADS) by ../models/downloadsBusy
 *   for the life of the app, not only while this page is open (the Kotlin window asked the download
 *   services themselves when it closed).
 */
import { useMemo } from "react";
import { createPortal } from "react-dom";
import { downloadLines } from "../models/downloadLines";
import { watchDownloadsBusy } from "../models/downloadsBusy";
import { availableModels } from "../models/library";
import { DownloadsStrip } from "../models/ModelDownloadsStrip";
import { ModelsPage, ModelsSearchField, useModelsPageState } from "../models/ModelsPage";
import { useModelsData } from "../models/useModelsData";

// SettingsPopup imports this page at start, so the quit dialog knows of downloads from then on.
watchDownloadsBusy();

export interface ModelsTabProps {
  /**
   * The part of the deep link after "models/". "code" is Models opened from Code's model menu
   * (ModelsPageState.forCode): it shows the models Code can use (the Code kind), and picking one
   * calls `onPicked`. Undefined for a plain "models".
   */
  initialSection?: string;
  /** Closes Settings: after a model was picked for Code from "models/code". */
  onPicked: () => void;
  /** The header row's slot for the downloads strip and the search field; null until mounted. */
  headerSlot: HTMLElement | null;
}

export function ModelsTab({ initialSection, onPicked, headerSlot }: ModelsTabProps) {
  const state = useModelsPageState(initialSection === "code");
  const data = useModelsData();
  const library = useMemo(() => availableModels(data.catalog, data.installed), [data.catalog, data.installed]);
  const lines = useMemo(
    () =>
      downloadLines(
        library,
        data.downloads.downloadingModels,
        data.downloads.pausedModels,
        data.downloads.downloadingProgress,
        data.runtimeDownloads,
        (id) => data.catalog?.models.find((m) => m.id === id)?.displayName ?? null,
      ),
    [library, data.downloads, data.runtimeDownloads, data.catalog],
  );

  return (
    <>
      {headerSlot &&
        createPortal(
          <>
            <DownloadsStrip lines={lines} />
            <ModelsSearchField state={state} />
            <span className="nk-models-header__gap" />
          </>,
          headerSlot,
        )}
      <ModelsPage state={state} data={data} onPicked={onPicked} />
    </>
  );
}
