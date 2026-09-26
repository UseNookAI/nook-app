/**
 * Models › Workers (WorkerModelsView.kt): which installed model does Nook Code's work, and which
 * one transcribes the voice prompt. "Automatic" keeps Nook's default; any other choice is
 * remembered in runtime/workers.json and used from the next call. Then the worker's web access.
 */
import { useEffect, useMemo, useState } from "react";
import { messageOf } from "../../../api/ipc";
import { workersCurrent, workersPreferences, workersSet, type Catalog, type LocalModel } from "../../../api/models";
import { runtimeStatus, type SpeedProbeResult } from "../../../api/runtime";
import { SettingsDropdown } from "../components";
import { WebAccessSetting } from "./WebAccessSetting";
import { AUTOMATIC, candidatesFor, idForLabel, labelFor, selectedLabel, taskDescription, WORKER_TASKS } from "./workers";
import "./models.css";

export function WorkerModelsView({ catalog, installed }: { catalog: Catalog | null; installed: LocalModel[] }) {
  const [prefs, setPrefs] = useState<Record<string, string>>({});
  const [current, setCurrent] = useState<Record<string, string>>({});
  const [probes, setProbes] = useState<SpeedProbeResult[]>([]);
  const [message, setMessage] = useState<string | null>(null);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let alive = true;
    Promise.all([workersPreferences(), workersCurrent(), runtimeStatus().then((s) => s.probes).catch(() => [] as SpeedProbeResult[])])
      .then(([p, c, pr]) => {
        if (!alive) return;
        setPrefs(p);
        setCurrent(c);
        setProbes(pr);
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [tick, installed]);

  // ModelCatalog.workerModels(): the catalog models with the worker capability.
  const workerIds = useMemo(
    () => new Set((catalog?.models ?? []).filter((m) => m.capabilities.includes("worker")).map((m) => m.id)),
    [catalog],
  );

  return (
    <div className="nk-models__scroll nk-workers">
      <div className="body2 nk-workers__intro">
        The local models Nook Code runs on this machine: the one that writes the code, and the one that hears the voice prompt.
      </div>
      {message && <div className="caption nk-models__error">{message}</div>}
      {WORKER_TASKS.map((t) => {
        const candidates = candidatesFor(t, installed, workerIds);
        return (
          <SettingsDropdown
            key={t.task}
            title={t.title}
            description={taskDescription(t, candidates, current[t.task], probes)}
            options={[AUTOMATIC, ...candidates.map(labelFor)]}
            selectedValue={selectedLabel(candidates, prefs[t.task])}
            onOptionSelect={(label) => {
              workersSet(t.task, idForLabel(candidates, label))
                .then(() => setMessage(null))
                .catch((e) => setMessage(`Could not save: ${messageOf(e)}`))
                .finally(() => setTick((n) => n + 1));
            }}
          />
        );
      })}
      <WebAccessSetting />
    </div>
  );
}
