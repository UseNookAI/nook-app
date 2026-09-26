/**
 * Minimise, maximise and close as quiet glyphs (NookWindowControls.kt); close turns warm red on
 * hover. Maximise shows two offset squares (restore) while the window is maximised.
 *
 * The Kotlin window sized itself to the monitor's work area instead of letting the OS maximise it,
 * because an undecorated Swing window maximised by the OS pushed its bottom strip off screen; a
 * Tauri window maximises within the work area, so the plain toggle is used here.
 */
import { Icon } from "../components/Icon";
import { minimizeWindow, toggleMaximizeWindow, useIsMaximized } from "./window";
import "./shell.css";

export function WindowControls({ onClose }: { onClose: () => void }) {
  const maximized = useIsMaximized();
  return (
    <div className="nk-window-controls">
      <button type="button" className="nk-window-control" aria-label="Minimise" onClick={minimizeWindow}>
        <span className="nk-glyph-minimise" />
      </button>
      <button
        type="button"
        className="nk-window-control"
        aria-label={maximized ? "Restore" : "Maximise"}
        onClick={toggleMaximizeWindow}
      >
        {maximized ? (
          <span className="nk-glyph-restore">
            <span className="nk-glyph-restore__back" />
            <span className="nk-glyph-restore__front" />
          </span>
        ) : (
          <span className="nk-glyph-maximise" />
        )}
      </button>
      <button type="button" className="nk-window-control nk-window-control--danger" aria-label="Close" onClick={onClose}>
        <Icon name="close" size={14} />
      </button>
    </div>
  );
}
