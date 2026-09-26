/** CodeControls.kt: the open/closed mark and toggle for everything that folds in Code mode. */
import "./code.css";

/**
 * The open/closed mark for everything that folds in Code mode: an arrow in a small circle,
 * pointing right when closed and down when open. Hover comes from the enclosing `.nc-fold`.
 */
export function FoldArrow({ open, size = 18 }: { open: boolean; size?: number }) {
  return (
    <span className="nc-fold-arrow" style={{ width: size, height: size }}>
      <svg
        viewBox="0 0 18 18"
        width={size}
        height={size}
        className={open ? "nc-fold-arrow__chevron nc-fold-arrow__chevron--open" : "nc-fold-arrow__chevron"}
        aria-hidden
      >
        <path d="M5.76 7.56 L9 10.8 L12.24 7.56" fill="none" stroke="currentColor" strokeWidth={1.4} strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    </span>
  );
}

/** "Show 12 steps", "Hide diff": a [FoldArrow] and its words, one click target. */
export function FoldToggle({ text, open, onClick }: { text: string; open: boolean; onClick: () => void }) {
  return (
    <button type="button" className="nc-fold" onClick={onClick} aria-expanded={open}>
      <FoldArrow open={open} />
      <span className="button-text">{text}</span>
    </button>
  );
}
