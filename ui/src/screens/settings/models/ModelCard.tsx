/**
 * ModelCard.kt: a model as a small box of one height for every card (164 px, so a grid of them
 * reads as a grid): name, one line of facts (size, kind, what it needs), two lines of description,
 * then chips on the left and actions on the right of the bottom row. A download in flight shows
 * its bar where the chips sit. Also the grid's section label and the download bar the cards and
 * the Models title share.
 */
import type { ReactNode } from "react";
import { Chip } from "../../../components/Activity";
import type { ChipSpec } from "./library";
import { percent } from "./hub";
import "./models.css";

export function ModelCard({
  title,
  meta,
  metaWarning = false,
  description,
  chips = [],
  progress = null,
  progressPaused = false,
  actionsOnHover = false,
  actions,
}: {
  title: string;
  meta: string;
  /** The line of facts in the warning colour: a file Nook cannot run. */
  metaWarning?: boolean;
  description: string | null;
  chips?: readonly ChipSpec[];
  /** 0..1 while downloading or paused; null otherwise. */
  progress?: number | null;
  progressPaused?: boolean;
  /** The actions show only while the pointer is over the card (an installed model's lone Delete). */
  actionsOnHover?: boolean;
  actions?: ReactNode;
}) {
  return (
    <div className={actionsOnHover ? "nk-model-card nk-model-card--hover-actions" : "nk-model-card"}>
      <div className="subtitle1 nk-model-card__title" title={title}>
        {title}
      </div>
      <div className={metaWarning ? "caption nk-model-card__meta nk-model-card__meta--warning" : "caption nk-model-card__meta"}>{meta}</div>
      <div className="body2 nk-model-card__description">{description ?? ""}</div>
      {/* At least this much air between two lines of description and the buttons. */}
      <div className="nk-model-card__spacer" />
      <div className="nk-model-card__bottom">
        {progress != null ? (
          <DownloadBar progress={progress} paused={progressPaused} className="nk-model-card__fill" />
        ) : (
          <FittingChips chips={chips} />
        )}
        <div className="nk-model-card__actions">{actions}</div>
      </div>
    </div>
  );
}

/**
 * The chips, left to right, as many as fit whole beside the buttons; the rest are left out rather
 * than squeezed into broken shapes (they wrap onto a second line that is clipped). The first chip
 * is the one that matters most.
 */
function FittingChips({ chips }: { chips: readonly ChipSpec[] }) {
  return (
    <div className="nk-fitting-chips">
      {chips.map(([text, accent]) => (
        <Chip key={text} text={text} accent={accent} />
      ))}
    </div>
  );
}

/** A small label above a run of cards: its name and how many. */
export function ModelSectionLabel({ text, count }: { text: string; count: number }) {
  return (
    <div className="nk-model-section">
      <span className="subtitle2 nk-model-section__text">{text}</span>
      <span className="numeric nk-model-section__count">{count}</span>
    </div>
  );
}

/** A download's bar and its percent, greyed while paused. The cards and the Models title both use it. */
export function DownloadBar({ progress, paused, className }: { progress: number; paused: boolean; className?: string }) {
  const p = Math.max(0, Math.min(1, progress));
  return (
    <div className={["nk-download-bar", className ?? ""].filter(Boolean).join(" ")}>
      <div className="nk-download-bar__track">
        <div
          className={paused ? "nk-download-bar__fill nk-download-bar__fill--paused" : "nk-download-bar__fill"}
          style={{ width: `${p * 100}%` }}
        />
      </div>
      <span className="numeric nk-download-bar__percent">{percent(p)}</span>
    </div>
  );
}
