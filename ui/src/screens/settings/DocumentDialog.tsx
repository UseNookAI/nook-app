/**
 * A bundled HTML document (the licence, the third-party notices) in a window-sized sheet. The
 * Kotlin app copied the file under the Nook home and handed it to the default browser; here the
 * core returns the HTML (app_eula, app_notices) and it is shown in place, in a sandboxed frame that
 * runs no script. Web links in it open in the default browser.
 */
import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { openUrl } from "../../api/app";
import { messageOf } from "../../api/ipc";
import { IconButton } from "../../components/Button";
import { Spinner } from "../../components/Spinner";
import "./settings.css";

export function DocumentDialog({ title, load, onDismiss }: { title: string; load: () => Promise<string>; onDismiss: () => void }) {
  const [html, setHtml] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    load()
      .then((h) => alive && setHtml(h))
      .catch((e) => alive && setError(messageOf(e)));
    return () => {
      alive = false;
    };
  }, [load]);

  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") onDismiss();
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [onDismiss]);

  // Same-origin without scripts, so the page can be read (for its links) but can run nothing.
  const wireLinks = (frame: HTMLIFrameElement) => {
    const doc = frame.contentDocument;
    if (!doc) return;
    // The light theme is white: the documents' own Floral White page gives way to it.
    const page = getComputedStyle(document.documentElement).getPropertyValue("--document-bg").trim();
    if (page && doc.body) doc.body.style.background = page;
    doc.addEventListener("click", (e) => {
      const a = (e.target as Element | null)?.closest?.("a[href]");
      const href = a?.getAttribute("href") ?? "";
      if (/^(https?:|mailto:)/i.test(href)) {
        e.preventDefault();
        openUrl(href).catch(() => {});
      }
    });
  };

  return createPortal(
    <div
      className="nk-scrim"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onDismiss();
      }}
    >
      <div className="nk-document" role="dialog" aria-label={title}>
        <div className="nk-document__header">
          <span className="h6 nk-document__title">{title}</span>
          <IconButton icon="close" iconSize={12} title="Close" onClick={onDismiss} />
        </div>
        {html != null ? (
          <iframe
            className="nk-document__frame"
            title={title}
            sandbox="allow-same-origin"
            srcDoc={html}
            onLoad={(e) => wireLinks(e.currentTarget)}
          />
        ) : (
          <div className="nk-document__status body2">{error ? `Could not open ${title.toLowerCase()}: ${error}` : <Spinner size={24} />}</div>
        )}
      </div>
    </div>,
    document.body,
  );
}
