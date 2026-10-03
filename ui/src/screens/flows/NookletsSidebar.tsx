/**
 * The sidebar on the Nooklets page: the way to the finder and every Nooklet (the open one lit),
 * then the recent runs, newest first: translations, transcripts, summaries, read-alouds and
 * conversions, each opening its Nooklet. Folded, the Nooklets show as their icons.
 */
import { useEffect, useState, useSyncExternalStore } from "react";
import { convertJobs, onConvert, type Job } from "../../api/convert";
import { flowsRuns, onFlows, type Run } from "../../api/flows";
import { LiveDot } from "../../components/Activity";
import { ago } from "../../components/activityFormat";
import { Icon } from "../../components/Icon";
import { FlowMemory, kindOfFlow, NOOKLETS, nookletOf, type FlowKind } from "./memory";

/** How many runs the sidebar lists. */
const RECENT = 12;

/** One run in the list: a flow run or a conversion, as the sidebar shows it. */
export interface RecentRun {
  id: string;
  kind: FlowKind;
  name: string;
  at: number;
  going: boolean;
  failed: boolean;
}

export function recentRuns(runs: Run[], jobs: Job[], limit = RECENT): RecentRun[] {
  const fromRuns = runs.flatMap((r): RecentRun[] => {
    const kind = kindOfFlow(r.flow);
    if (!kind) return [];
    return [{ id: r.id, kind, name: r.inputName, at: r.createdAt, going: r.status === "QUEUED" || r.status === "RUNNING", failed: r.status === "FAILED" }];
  });
  const fromJobs = jobs.map(
    (j): RecentRun => ({
      id: j.id,
      kind: "CONVERT",
      name: j.items.length > 1 ? `${j.items[0].name} and ${j.items.length - 1} more` : (j.items[0]?.name ?? "Conversion"),
      at: j.at,
      going: j.status === "WAITING" || j.status === "CONVERTING",
      failed: j.status === "FAILED",
    }),
  );
  return [...fromRuns, ...fromJobs].sort((a, b) => b.at - a.at).slice(0, limit);
}

/** The runs of the flows and the converter, read again when either says something changed. */
function useRecentRuns(): RecentRun[] {
  const [list, setList] = useState<RecentRun[]>([]);
  useEffect(() => {
    let alive = true;
    let timer: number | undefined;
    const read = () =>
      Promise.all([flowsRuns().catch(() => [] as Run[]), convertJobs().catch(() => [] as Job[])]).then(([runs, jobs]) => {
        if (alive) setList(recentRuns(runs, jobs));
      });
    const soon = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(read, 400);
    };
    void read();
    const offFlows = onFlows(soon);
    const offConvert = onConvert(soon);
    return () => {
      alive = false;
      window.clearTimeout(timer);
      offFlows();
      offConvert();
    };
  }, []);
  return list;
}

export function NookletsSidebar({ isCollapsed }: { isCollapsed: boolean }) {
  const open = useSyncExternalStore(FlowMemory.subscribe, FlowMemory.get);
  const runs = useRecentRuns();

  const item = (key: string, icon: string, title: string, caption: string, active: boolean, onClick: () => void, find = false) => (
    <button
      key={key}
      type="button"
      className={["nc-nooklet-item", active ? "nc-nooklet-item--active" : "", find ? "nc-nooklet-item--find" : ""].filter(Boolean).join(" ")}
      aria-current={active ? "page" : undefined}
      title={title}
      onClick={onClick}
    >
      <span className="nc-nooklet-item__icon">
        <Icon name={icon} size={15} />
      </span>
      {!isCollapsed && (
        <span className="nc-nooklet-item__text">
          <span className="body2 nc-ellipsis nc-nooklet-item__title">{title}</span>
          <span className="caption text-tertiary nc-ellipsis">{caption}</span>
        </span>
      )}
    </button>
  );

  return (
    <div className="nc-sidebar__scroll">
      {!isCollapsed && <div className="overline text-tertiary nc-sidebar__heading">Nooklets</div>}
      <div className="nc-sidebar__list">
        {item("find", "search", "Ask for a Nooklet", "Say what you want done", open === "HOME", () => FlowMemory.set("HOME"), true)}
        {NOOKLETS.map((n) => item(n.kind, n.icon, n.title, n.blurb, open === n.kind, () => FlowMemory.set(n.kind)))}
      </div>
      {!isCollapsed && (
        <>
          <div className="overline text-tertiary nc-sidebar__heading">Recent</div>
          <div className="nc-sidebar__list">
            {runs.length === 0 && <div className="body2 text-tertiary nc-sidebar__empty">What your Nooklets make appears here.</div>}
            {runs.map((r) => (
              <button key={r.id} type="button" className="nc-session-item nc-run-item" title={r.name} onClick={() => FlowMemory.set(r.kind)}>
                <span className="nc-session-item__text">
                  <span className="body2 nc-session-item__title">{r.name}</span>
                  <span className="caption text-tertiary nc-ellipsis">
                    {nookletOf(r.kind).title} · {r.going ? "working" : r.failed ? "failed" : ago(r.at)}
                  </span>
                </span>
                {r.going && (
                  <span className="nc-session-item__end">
                    <LiveDot />
                  </span>
                )}
              </button>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
