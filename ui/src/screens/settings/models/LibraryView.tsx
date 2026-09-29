/**
 * Models › Library (AiModelsSettingsView.kt's LibraryModelsView): the curated catalog and the
 * installed models as a grid of cards in three sections, Installing, Installed and Available,
 * filtered by the header's search field and the kind. Above the grid, the GPU and what is loaded.
 *
 * Installing cards pause, resume and cancel; installed ones delete (never a model shared from the
 * installed Nook: that is deleted from Nook itself) and switch Code to them; available ones
 * download. From Code's model menu ("models/code") picking a model for Code closes Settings.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { codeSetWorker } from "../../../api/code";
import { messageOf } from "../../../api/ipc";
import { modelsCancel, modelsDelete, modelsDownload, modelsPause, modelsResume } from "../../../api/models";
import type { RuntimeStatus } from "../../../api/runtime";
import { Button } from "../../../components/Button";
import { Dialog } from "../../../components/Dialog";
import { useSnackbar } from "../../../components/Snackbar";
import { Spinner } from "../../../components/Spinner";
import { useCodeSnapshot } from "../../code/useCode";
import { SettingsAction } from "../components";
import { useRuntimeStatus } from "../runtime/useRuntimeStatus";
import {
  availableModels,
  chipsFor,
  librarySections,
  metaFor,
  uniqueKey,
  type AiModelDto,
  type ChipContext,
  type Kind,
} from "./library";
import { ModelCard, ModelSectionLabel } from "./ModelCard";
import type { ModelsData } from "./useModelsData";
import "./models.css";
import { gpuMemory } from "../../../shell/platform";

const GIB = 1024 * 1024 * 1024;

/** "NVIDIA GeForce RTX 4060 · 1.1 of 8 GB VRAM free · Loaded: gpt-oss 20B, Qwen3 8B". */
export function gpuLine(st: RuntimeStatus): string {
  const gpu = st.devices[0];
  const gpuText = gpu
    ? `${gpu.name} · ${(gpu.freeBytes / GIB).toFixed(1)} of ${(gpu.totalBytes / GIB).toFixed(0)} GB ${gpuMemory} free`
    : "No GPU reading yet";
  const loaded = st.engines
    .filter((e) => e.state === "READY")
    .map((e) => e.displayName)
    .join(", ");
  return gpuText + (loaded.trim() !== "" ? ` · Loaded: ${loaded}` : "");
}

export function LibraryView({
  data,
  query,
  kind,
  forCode,
  onPicked,
}: {
  data: ModelsData;
  query: string;
  kind: Kind;
  forCode: boolean;
  onPicked: () => void;
}) {
  const { say } = useSnackbar();
  const code = useCodeSnapshot();
  // Refresh the runtime snapshot (GPU budget, resident engines) while the page is open.
  const [status] = useRuntimeStatus(3000);
  const [useError, setUseError] = useState<string | null>(null);
  const [modelToDelete, setModelToDelete] = useState<AiModelDto | null>(null);
  const [modelToStop, setModelToStop] = useState<AiModelDto | null>(null);
  const scroller = useRef<HTMLDivElement>(null);

  const { catalog, installed, downloads } = data;
  const models = useMemo(() => availableModels(catalog, installed), [catalog, installed]);
  const installedNames = useMemo(() => new Set(installed.map((m) => m.id)), [installed]);
  // The models Code's own menu offers (every installed chat model), and the one it works with.
  const codeChoices = useMemo(() => new Set(code.workers.map((w) => w.id)), [code.workers]);
  const codeWorker = code.workerId;
  const sections = useMemo(
    () => librarySections(models, installedNames, downloads, query, kind, codeChoices),
    [models, installedNames, downloads, query, kind, codeChoices],
  );

  const gpu = status?.devices[0];
  const chipContext: ChipContext = {
    codeWorker,
    residentIds: new Set(status?.engines.filter((e) => e.state === "READY").map((e) => e.modelId) ?? []),
    defaultChatModel: catalog?.defaultChatModel ?? null,
    gpuTotalGb: gpu ? gpu.totalBytes / GIB : null,
    probes: status?.probes ?? [],
  };

  // A download that has just started jumps the grid to the top, where it lands.
  const previousInstalling = useRef(sections.installing.length);
  useEffect(() => {
    if (sections.installing.length > previousInstalling.current) scroller.current?.scrollTo({ top: 0, behavior: "smooth" });
    previousInstalling.current = sections.installing.length;
  }, [sections.installing.length]);

  const act = (block: () => Promise<unknown>) => {
    block()
      .catch((e) => say(messageOf(e)))
      .finally(data.reload);
  };

  const chooseForCode = (model: AiModelDto) => {
    codeSetWorker(model.model)
      .then(() => {
        setUseError(null);
        if (forCode) onPicked();
      })
      .catch((e) => setUseError(messageOf(e) || "Code could not switch to that model."));
  };

  const loading = catalog == null || (downloads.isLoading && models.length === 0);
  const nothing = sections.installing.length === 0 && sections.installed.length === 0 && sections.available.length === 0;

  return (
    <div className="nk-models__body">
      {status && <div className="caption nk-models__note">{gpuLine(status)}</div>}
      {useError && <div className="caption nk-models__error">{useError}</div>}

      {loading ? (
        <div className="nk-models__center">
          <Spinner size={24} />
        </div>
      ) : nothing ? (
        <div className="nk-models__center body2 text-secondary">{models.length === 0 ? "No models in the catalog yet." : "Nothing matches."}</div>
      ) : (
        <div ref={scroller} className="nk-models__scroll">
          <div className="nk-model-grid">
            {sections.installing.length > 0 && <ModelSectionLabel text="Installing" count={sections.installing.length} />}
            {sections.installing.map((model) => {
              const key = uniqueKey(model);
              const paused = downloads.pausedModels.includes(key);
              const stopping = downloads.stoppingModels.includes(key);
              return (
                <ModelCard
                  key={key}
                  title={model.fullName ?? model.model}
                  meta={metaFor(model)}
                  description={model.description}
                  progress={downloads.downloadingProgress[key] ?? 0}
                  progressPaused={paused}
                  actions={
                    <>
                      {stopping ? (
                        <span className="caption text-secondary">Cancelling…</span>
                      ) : paused ? (
                        <SettingsAction text="Resume" onClick={() => act(() => modelsResume(key))} />
                      ) : (
                        <SettingsAction text="Pause" onClick={() => act(() => modelsPause(key))} />
                      )}
                      {!stopping && (
                        <SettingsAction
                          text="Cancel"
                          danger
                          onClick={() => {
                            if (downloads.textDownloadingCount > 1) setModelToStop(model);
                            else act(() => modelsCancel(key));
                          }}
                        />
                      )}
                    </>
                  }
                />
              );
            })}

            {sections.installed.length > 0 && <ModelSectionLabel text="Installed" count={sections.installed.length} />}
            {sections.installed.map((model) => {
              const key = uniqueKey(model);
              const deleting = downloads.deletingModels.includes(key);
              // Any model Code's own menu offers: every installed chat model, tested or not.
              const canUse = !deleting && codeChoices.has(model.model) && model.model !== codeWorker;
              // A model from the installed Nook's folder is never deleted here.
              const canDelete = !model.shared;
              return (
                <ModelCard
                  key={key}
                  title={model.fullName ?? model.model}
                  meta={metaFor(model)}
                  metaWarning={model.unsupported != null}
                  description={model.description}
                  chips={chipsFor(model, true, chipContext)}
                  // A model Code could switch to shows its buttons outright, and so does a file Nook
                  // cannot run (Delete is all it is good for); otherwise Delete alone waits for the pointer.
                  actionsOnHover={!deleting && !canUse && model.unsupported == null}
                  actions={
                    deleting ? (
                      <span className="caption text-secondary">Deleting…</span>
                    ) : (
                      <>
                        {canDelete && <SettingsAction text="Delete" danger onClick={() => setModelToDelete(model)} />}
                        {canUse && <SettingsAction text="Use for Code" primary onClick={() => chooseForCode(model)} />}
                      </>
                    )
                  }
                />
              );
            })}

            {sections.available.length > 0 && <ModelSectionLabel text="Available" count={sections.available.length} />}
            {sections.available.map((model) => (
              <ModelCard
                key={uniqueKey(model)}
                title={model.fullName ?? model.model}
                meta={metaFor(model)}
                description={model.description}
                chips={chipsFor(model, false, chipContext)}
                actions={<SettingsAction text="Download" primary onClick={() => act(() => modelsDownload(uniqueKey(model)))} />}
              />
            ))}
          </div>
        </div>
      )}

      {modelToStop && (
        <ConfirmDialog
          title="Stop this download?"
          body="Stopping it also cancels the other text-model downloads in progress."
          confirm="Stop"
          onConfirm={() => {
            const key = uniqueKey(modelToStop);
            setModelToStop(null);
            act(() => modelsCancel(key));
          }}
          onDismiss={() => setModelToStop(null)}
        />
      )}
      {modelToDelete && (
        <ConfirmDialog
          title={`Delete ${modelToDelete.fullName ?? modelToDelete.model}?`}
          body="The files are removed from this machine. You can download it again later."
          confirm="Delete"
          danger
          onConfirm={() => {
            const key = uniqueKey(modelToDelete);
            setModelToDelete(null);
            act(() => modelsDelete(key));
          }}
          onDismiss={() => setModelToDelete(null)}
        />
      )}
    </div>
  );
}

function ConfirmDialog({
  title,
  body,
  confirm,
  danger = false,
  onConfirm,
  onDismiss,
}: {
  title: string;
  body: string;
  confirm: string;
  danger?: boolean;
  onConfirm: () => void;
  onDismiss: () => void;
}) {
  return (
    <Dialog
      title={title}
      onDismiss={onDismiss}
      actions={
        <>
          <Button text="Cancel" variant="ghost" onClick={onDismiss} />
          <Button text={confirm} variant={danger ? "danger" : "primary"} onClick={onConfirm} />
        </>
      }
    >
      {body}
    </Dialog>
  );
}
