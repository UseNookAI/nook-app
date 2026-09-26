/**
 * VideoScreen.kt AviPlayer: plays an MJPEG AVI in a loop by decoding one JPEG per frame; a click
 * pauses and resumes. Only the newest clip plays by itself, so a long list does not keep the CPU
 * decoding; and, as the Compose list disposed the cards scrolled out of it, a card loads its file
 * only once it comes into view and stops decoding while it is out of view.
 */
import { useEffect, useRef, useState } from "react";
import { clipSrc } from "../../api/video";
import { Icon } from "../../components/Icon";
import { parseAvi, type AviFrames } from "./aviFrames";

type Loaded = { frames: AviFrames } | { error: string };

const decode = (frames: AviFrames, index: number) =>
  createImageBitmap(new Blob([frames.jpeg(index)], { type: "image/jpeg" }));

const sleep = (ms: number) => new Promise<void>((resolve) => window.setTimeout(resolve, ms));

/** Key it by the file: a new file starts over, as `remember(file)` did. */
export function AviPlayer({ file, autoPlay, aspect }: { file: string; autoPlay: boolean; aspect: number }) {
  const box = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [visible, setVisible] = useState(false);
  const [seen, setSeen] = useState(false);
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  const [playing, setPlaying] = useState(autoPlay);
  const [drawn, setDrawn] = useState(false);
  const index = useRef(0);

  useEffect(() => {
    const el = box.current;
    if (!el || typeof IntersectionObserver === "undefined") {
      setVisible(true);
      setSeen(true);
      return;
    }
    const observer = new IntersectionObserver(([entry]) => {
      setVisible(entry.isIntersecting);
      if (entry.isIntersecting) setSeen(true);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!seen) return;
    let alive = true;
    (async () => {
      try {
        const res = await fetch(clipSrc(file));
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const frames = parseAvi(await res.arrayBuffer());
        if (alive) setLoaded({ frames });
      } catch (e) {
        if (alive) setLoaded({ error: e instanceof Error ? e.message : String(e) });
      }
    })();
    return () => {
      alive = false;
    };
  }, [file, seen]);

  const draw = (bitmap: ImageBitmap) => {
    const c = canvas.current;
    const ctx = c?.getContext("2d");
    if (c && ctx) {
      if (c.width !== bitmap.width || c.height !== bitmap.height) {
        c.width = bitmap.width;
        c.height = bitmap.height;
      }
      ctx.drawImage(bitmap, 0, 0);
    }
    bitmap.close();
  };

  const frames = loaded && "frames" in loaded ? loaded.frames : null;

  // The first frame, playing or not.
  useEffect(() => {
    if (!frames || drawn) return;
    let alive = true;
    decode(frames, index.current).then(
      (bitmap) => {
        if (!alive) return bitmap.close();
        draw(bitmap);
        setDrawn(true);
      },
      (e) => alive && setLoaded({ error: e instanceof Error ? e.message : String(e) }),
    );
    return () => {
      alive = false;
    };
  }, [frames, drawn]);

  useEffect(() => {
    if (!frames || !drawn || !playing || !visible || frames.count < 2) return;
    let stopped = false;
    (async () => {
      const frameMs = 1000 / frames.fps;
      let next = performance.now() + frameMs;
      while (!stopped) {
        const i = (index.current + 1) % frames.count;
        const bitmap = await decode(frames, i).catch(() => null);
        const wait = next - performance.now();
        if (wait > 0 && !stopped) await sleep(wait);
        if (stopped) {
          bitmap?.close();
          return;
        }
        index.current = i;
        if (bitmap) draw(bitmap);
        next += frameMs;
        // Fell far behind (the window was busy): pick up from now instead of racing to catch up.
        if (performance.now() - next > frameMs * 4) next = performance.now() + frameMs;
      }
    })();
    return () => {
      stopped = true;
    };
  }, [frames, drawn, playing, visible]);

  const failed = loaded != null && "error" in loaded;
  return (
    <div
      ref={box}
      className="vd-player"
      style={{ aspectRatio: String(aspect) }}
      onClick={() => setPlaying((p) => !p)}
      role="button"
      aria-label={playing ? "Pause" : "Play"}
    >
      <canvas ref={canvas} className="vd-player__canvas" />
      {failed ? (
        <div className="vd-player__error caption">This clip can't play here. Use Open to play it in your video player.</div>
      ) : (
        frames &&
        !playing && (
          <div className="vd-player__play">
            <Icon name="resume" size={24} />
          </div>
        )
      )}
    </div>
  );
}
