/**
 * What a flow still needs, as one download: what it is and its size with one button; while it
 * runs, what is coming down, a progress bar and Stop; when it failed, why and Try again. The
 * translator and the PDF editor show their downloads with it alike.
 */
import type { Install } from "../../api/flows";
import { Button } from "../../components/Button";
import { ProgressBar } from "../../components/Spinner";
import { bytesText } from "./format";

export function DownloadLine({
  text,
  bytes,
  install,
  onDownload,
  onStop,
  onRetry,
}: {
  /** What the download brings, before it starts: "One-time download: …". */
  text: string;
  /** Its size, on the button. */
  bytes: number;
  install: Install | null;
  onDownload: () => void;
  onStop: () => void;
  onRetry: () => void;
}) {
  if (install?.error) {
    return (
      <div className="fl-setup">
        <span className="caption text-warning fl-setup__text">{install.error}</span>
        <Button text="Try again" variant="secondary" compact onClick={onRetry} />
      </div>
    );
  }
  if (install) {
    const progress = install.total > 0 ? install.done / install.total : null;
    return (
      <div className="fl-setup fl-setup--running">
        <div className="fl-setup__text">
          <span className="caption text-secondary">
            Downloading {install.what} · {bytesText(install.done)} of {bytesText(install.total)}
          </span>
          <div className="fl-bar">
            <ProgressBar progress={progress} height={6} />
          </div>
        </div>
        <Button text="Stop" variant="secondary" compact onClick={onStop} />
      </div>
    );
  }
  return (
    <div className="fl-setup">
      <span className="caption text-secondary fl-setup__text">{text}</span>
      <Button text={`Download ${bytesText(bytes)}`} icon="download" iconPosition="start" variant="accent" compact onClick={onDownload} />
    </div>
  );
}
