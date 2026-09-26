/**
 * ModelDownloadsStrip.kt: beside the Models title while a model downloads: its name, its bar and
 * its percent, and how many more are coming when there are several. Nothing while none is. The
 * lines come from [downloadLines] (the library's downloads, then the runtime's own).
 */
import { Icon } from "../../../components/Icon";
import type { DownloadLine } from "./downloadLines";
import { DownloadBar } from "./ModelCard";
import "./models.css";

/** The strip itself, drawn from `lines`: the first download in full, the rest as a count. */
export function DownloadsStrip({ lines }: { lines: readonly DownloadLine[] }) {
  const first = lines[0];
  return (
    <div className="nk-downloads-strip">
      {first && (
        <div className="nk-downloads-strip__row" title={lines.map((l) => l.name).join("\n")}>
          <Icon name="download" size={14} className="nk-downloads-strip__icon" />
          <span className="body2 nk-downloads-strip__name">{first.name}</span>
          {lines.length > 1 && <span className="numeric nk-downloads-strip__more">+{lines.length - 1}</span>}
          <DownloadBar progress={first.progress} paused={first.paused} className="nk-downloads-strip__bar" />
        </div>
      )}
    </div>
  );
}
