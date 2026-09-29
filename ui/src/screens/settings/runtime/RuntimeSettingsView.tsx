/**
 * Settings › Runtime (RuntimeSettingsView.kt): a live view of the runtime: engine versions, each
 * GPU's memory, the measured speed of each model on this machine, loaded models with their queues
 * (pin, unload), downloads in progress and the recent event feed. Read every two seconds while
 * open, and at once on each runtime event.
 */
import { messageOf } from "../../../api/ipc";
import { BACKEND_LABELS, runtimePin, runtimeUnload, type EngineInfo, type SpeechInfo } from "../../../api/runtime";
import { Chip } from "../../../components/Activity";
import { useSnackbar } from "../../../components/Snackbar";
import { SettingsAction, SettingsGroup, SettingsItemRow, SettingsRowDivider, SettingsView } from "../components";
import {
  downloadPercent,
  engineLayout,
  engineQueue,
  eventFailed,
  eventLine,
  gpuDriverText,
  gpuUsedShare,
  gpuUsedText,
  probeDescription,
  probeTitle,
  stateText,
} from "./runtimeFormat";
import { useRuntimeStatus } from "./useRuntimeStatus";
import "./runtime.css";
import { isMac } from "../../../shell/platform";

export function RuntimeSettingsView() {
  const [st, refresh] = useRuntimeStatus(2000);
  const { say } = useSnackbar();

  if (st == null) {
    return (
      <SettingsView>
        <div className="body2 text-secondary">Reading runtime status…</div>
      </SettingsView>
    );
  }

  const act = (block: () => Promise<unknown>) => {
    block()
      .catch((e) => say(messageOf(e)))
      .finally(refresh);
  };

  const nothingLoaded = st.engines.length === 0 && st.speechEngines.length === 0 && !st.imageBusy;
  const downloads = Object.entries(st.downloads);

  return (
    <SettingsView>
      <SettingsGroup title="Engines">
        <ValueRow label="Backend" value={BACKEND_LABELS[st.backend] ?? st.backend} />
        <SettingsRowDivider />
        <ValueRow label="Text · llama.cpp" value={st.engineInstalled ? st.engineVersion : "not installed"} />
        <SettingsRowDivider />
        <ValueRow
          label="Speech · whisper.cpp"
          value={st.speechEngineInstalled ? st.speechEngineVersion : "installs with the first speech model"}
        />
      </SettingsGroup>

      <SettingsGroup title="GPU memory">
        {st.devices.length === 0 && (
          <Note>
            {isMac
              ? "No GPU reading: Metal did not answer, so models run on the processor."
              : "No GPU reading yet. On an AMD or Intel card the memory is read through the Vulkan engine once it is installed; until then models fit themselves to the memory available."}
          </Note>
        )}
        {st.devices.map((gpu, index) => {
          const { share, nearlyFull } = gpuUsedShare(gpu);
          return (
            <div key={gpu.index}>
              {index > 0 && <SettingsRowDivider />}
              <div className="nk-runtime-gpu">
                <div className="nk-runtime-gpu__head">
                  <span className="body1 nk-runtime__name">{gpu.name}</span>
                  <span className="body2 text-secondary">{gpuUsedText(gpu)}</span>
                </div>
                <div className="nk-runtime-gpu__track">
                  <div
                    className={nearlyFull ? "nk-runtime-gpu__fill nk-runtime-gpu__fill--full" : "nk-runtime-gpu__fill"}
                    style={{ width: `${share * 100}%` }}
                  />
                </div>
                <div className="caption text-tertiary nk-runtime-gpu__driver">{gpuDriverText(gpu, st.budgetBytes)}</div>
              </div>
            </div>
          );
        })}
      </SettingsGroup>

      <SettingsGroup title="Speed on this machine">
        {/* The speed probe: measured once per model and driver, the number that says whether a worker is fast enough. */}
        {st.probes.length === 0 && (
          <Note>Measured the first time a chat model loads: a fixed 500-token prompt and 300-token answer, about thirty seconds.</Note>
        )}
        {st.probes.map((p, i) => (
          <div key={p.modelId}>
            {i > 0 && <SettingsRowDivider />}
            <SettingsItemRow title={probeTitle(p, st.engines)} description={probeDescription(p)} />
          </div>
        ))}
      </SettingsGroup>

      <SettingsGroup title="Loaded models">
        {nothingLoaded && <Note>Nothing loaded. A model loads on the first request and unloads after ten idle minutes.</Note>}
        {st.engines.map((e, i) => (
          <div key={e.modelId}>
            {i > 0 && <SettingsRowDivider />}
            <EngineRow
              e={e}
              onPin={() => act(() => runtimePin(e.modelId, !e.pinned))}
              onUnload={() => act(() => runtimeUnload(e.modelId))}
            />
          </div>
        ))}
        {st.speechEngines.map((w, i) => (
          <div key={w.modelId}>
            {(st.engines.length > 0 || i > 0) && <SettingsRowDivider />}
            <SpeechRow w={w} onUnload={() => act(() => runtimeUnload(w.modelId))} />
          </div>
        ))}
        {st.imageBusy && (
          <>
            {st.engines.length + st.speechEngines.length > 0 && <SettingsRowDivider />}
            <Note>Rendering an image now.</Note>
          </>
        )}
      </SettingsGroup>

      {downloads.length > 0 && (
        <SettingsGroup title="Downloads">
          {downloads.map(([id, p], i) => (
            <div key={id}>
              {i > 0 && <SettingsRowDivider />}
              <ValueRow label={id} value={downloadPercent(p)} />
            </div>
          ))}
        </SettingsGroup>
      )}

      <SettingsGroup title="Recent events">
        {st.recentEvents.length === 0 && <Note>Nothing yet.</Note>}
        <div className="nk-runtime-events selectable">
          {st.recentEvents.slice(0, 40).map((ev, i) => (
            <div key={`${ev.at}-${i}`} className={eventFailed(ev) ? "nk-runtime-events__line nk-runtime-events__line--failed" : "nk-runtime-events__line"}>
              {eventLine(ev)}
            </div>
          ))}
        </div>
      </SettingsGroup>
    </SettingsView>
  );
}

function EngineRow({ e, onPin, onUnload }: { e: EngineInfo; onPin: () => void; onUnload: () => void }) {
  return (
    <div className="nk-runtime-engine">
      <div className="nk-runtime-engine__text">
        <div className="nk-runtime-engine__title">
          <span className="body1 nk-runtime__name">{e.displayName}</span>
          <Chip text={stateText(e.state)} accent={e.state === "READY"} />
          {e.pinned && <Chip text="Pinned" icon="pin" />}
        </div>
        <div className="caption text-secondary nk-runtime-engine__layout">{engineLayout(e)}</div>
        <div className="caption text-tertiary">{engineQueue(e)}</div>
      </div>
      <div className="nk-runtime-engine__actions">
        <SettingsAction text={e.pinned ? "Unpin" : "Pin"} onClick={onPin} />
        <SettingsAction text="Unload" danger onClick={onUnload} />
      </div>
    </div>
  );
}

function SpeechRow({ w, onUnload }: { w: SpeechInfo; onUnload: () => void }) {
  return (
    <div className="nk-runtime-engine">
      <div className="nk-runtime-engine__text">
        <div className="nk-runtime-engine__title">
          <span className="body1 nk-runtime__name">{w.modelId}</span>
          <Chip text="Speech" />
        </div>
        <div className="caption text-tertiary">{w.inFlight} running</div>
      </div>
      <SettingsAction text="Unload" danger onClick={onUnload} />
    </div>
  );
}

function ValueRow({ label, value }: { label: string; value: string }) {
  return (
    <SettingsItemRow title={label}>
      <span className="body2 text-secondary">{value}</span>
    </SettingsItemRow>
  );
}

function Note({ children }: { children: string | string[] }) {
  return <div className="body2 text-secondary nk-runtime__note">{children}</div>;
}
