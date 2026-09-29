import { lazy, StrictMode, Suspense } from "react";
import { createRoot } from "react-dom/client";
import "./theme/theme.css";
import "./theme/shell.css";
import "./shell/shell.css";
import { installMocks } from "./api/mocks";
import { inTauri } from "./api/ipc";
import { App } from "./App";
import type { PickAreaView } from "./screens/capture/AreaPicker";

installMocks();

/**
 * What this window shows: the app, or one of the screen recorder's own windows, which Nook tells
 * by `window.__NOOK_VIEW__` as it opens them (in a browser, `?view=pick-area` or
 * `?view=capture-bar` stands in).
 */
type View = PickAreaView | { view: "capture-bar" };

function view(): View | null {
  const told = (window as unknown as { __NOOK_VIEW__?: View }).__NOOK_VIEW__;
  if (told) return told;
  if (inTauri) return null;
  const asked = new URLSearchParams(window.location.search).get("view");
  if (asked === "pick-area") return { view: "pick-area", screen: 65537, width: window.innerWidth * 2, height: window.innerHeight * 2 };
  if (asked === "capture-bar") return { view: "capture-bar" };
  return null;
}

const AreaPicker = lazy(() => import("./screens/capture/AreaPicker").then((m) => ({ default: m.AreaPicker })));
const CaptureBar = lazy(() => import("./screens/capture/CaptureBar").then((m) => ({ default: m.CaptureBar })));

const shown = view();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {shown?.view === "pick-area" ? (
      <Suspense fallback={null}>
        <AreaPicker view={shown} />
      </Suspense>
    ) : shown?.view === "capture-bar" ? (
      <Suspense fallback={null}>
        <CaptureBar />
      </Suspense>
    ) : (
      <App />
    )}
  </StrictMode>,
);
