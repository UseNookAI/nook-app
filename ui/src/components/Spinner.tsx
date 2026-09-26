import "./components.css";

/**
 * CircularProgressIndicator in the accent colour. `trackColor` draws the rest of the ring (a pale
 * accent by default; "transparent" for Compose's bare indeterminate arc).
 */
export function Spinner({
  size = 20,
  stroke = 2.5,
  color,
  trackColor,
}: {
  size?: number;
  stroke?: number;
  color?: string;
  trackColor?: string;
}) {
  return (
    <span
      className="nk-spinner"
      style={{
        width: size,
        height: size,
        borderWidth: stroke,
        ...(trackColor ? { borderColor: trackColor } : {}),
        borderTopColor: color ?? "var(--primary-variant)",
      }}
      role="progressbar"
    />
  );
}

/**
 * SimpleProgressBar.kt: a thin rounded track with a fill, progress 0..1 (null = indeterminate).
 * The update dialog uses it 8 px high; `color` and `trackColor` override the Hunter Green fill and
 * the border-grey track.
 */
export function ProgressBar({
  progress,
  height = 4,
  color,
  trackColor,
}: {
  progress: number | null;
  height?: number;
  color?: string;
  trackColor?: string;
}) {
  const width = progress == null ? "30%" : `${Math.max(0, Math.min(1, progress)) * 100}%`;
  return (
    <div className="nk-progress" style={{ height, background: trackColor }}>
      <div
        className={progress == null ? "nk-progress__bar nk-progress__bar--indeterminate" : "nk-progress__bar"}
        style={{ width, background: color }}
      />
    </div>
  );
}

/** The Kotlin name of ProgressBar. */
export const SimpleProgressBar = ProgressBar;
