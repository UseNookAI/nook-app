/**
 * The recording controls: a small bar on the recorded screen, above every window and kept out of
 * the recording itself. It shows the time, pauses a recording and stops it; dragged by its body.
 * When the recording has ended (stopped here, on Nook's page, or by itself) it asks Nook to
 * close it and come back.
 */
import { useEffect, useRef, useState } from "react";
import { captureState, capturePause, captureResume, captureStop, clock, IDLE, onCapture, type CaptureState } from "../../api/capture";
import { Icon } from "../../components/Icon";
import "./capture.css";

export function CaptureBar() {
  const [state, setState] = useState<CaptureState>(IDLE);
  const [known, setKnown] = useState(false);
  const closing = useRef(false);

  useEffect(() => {
    document.documentElement.classList.add("cb-page");
    let alive = true;
    captureState().then(
      (s) => {
        if (!alive) return;
        setState(s);
        setKnown(true);
      },
      () => undefined,
    );
    const off = onCapture((e) => {
      if (e.state) {
        setState(e.state);
        setKnown(true);
      }
    });
    return () => {
      alive = false;
      off();
      document.documentElement.classList.remove("cb-page");
    };
  }, []);

  // Over: this bar goes, and Nook comes back with the recording.
  useEffect(() => {
    if (known && state.phase === "idle" && !closing.current) {
      closing.current = true;
      void captureStop();
    }
  }, [known, state.phase]);

  const paused = state.phase === "paused";
  const busy = state.phase === "starting" || state.phase === "finishing";
  const label = busy ? (state.phase === "finishing" ? "Saving" : "Starting") : paused ? "Paused" : state.streaming ? "Live" : "Rec";
  return (
    <div className="cb" data-tauri-drag-region>
      <span className={paused || busy ? "cb-dot cb-dot--still" : "cb-dot"} data-tauri-drag-region />
      <span className="cb-label" data-tauri-drag-region>
        {label}
      </span>
      <span className="cb-clock" data-tauri-drag-region>
        {clock(state.seconds)}
      </span>
      <span className="cb-spacer" data-tauri-drag-region />
      {!state.streaming && (
        <button
          type="button"
          className="cb-button"
          title={paused ? "Resume" : "Pause"}
          disabled={busy}
          onClick={() => void (paused ? captureResume() : capturePause()).catch(() => undefined)}
        >
          <Icon name={paused ? "record" : "pause"} size={16} />
        </button>
      )}
      <button
        type="button"
        className="cb-button cb-button--stop"
        title={state.streaming ? "End the stream" : "Stop and save"}
        disabled={busy}
        onClick={() => {
          closing.current = true;
          void captureStop();
        }}
      >
        <Icon name="stop" size={16} />
      </button>
    </div>
  );
}
