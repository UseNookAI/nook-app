/**
 * The green square mark (FanMark.kt), which turns lavender and spins while Nook works the GPU,
 * faster the harder the card works. At rest it is drawn exactly as the logo.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { runtimeGpuLoad } from "../api/runtime";
import { FanMotor } from "./fanMotor";

/** The N of the mark, in the logo's 24-unit box (images/nook-primary-logo.svg). */
const N_PATH =
  "M8.90332 5.72949L14.6074 11.3975V5H19.5137V10.9062L15.6992 14.7012H19.5137V19.001H15.3125L14.6074 18.2979" +
  "L8.90332 12.627V19.001H4V13.0918L7.8125 9.2998H4V5H8.17285L8.90332 5.72949Z";
const N_CENTRE = { x: 11.75685, y: 12.0005 }; // the middle of the N's bounds, which it turns about
const TILE_RADIUS = 5.71831;
const SHUTTER = 40; // ms of travel each frame smears across
const POLL_MS = 500;

type Rgb = [number, number, number];

function rgb(value: string, fallback: Rgb): Rgb {
  const m = /^#?([0-9a-f]{6})$/i.exec(value.trim());
  if (!m) return fallback;
  const n = parseInt(m[1], 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

/** Green to lavender, mixed in sRGB as the site mixes them. */
function warm(from: Rgb, to: Rgb, k: number): string {
  const c = from.map((v, i) => Math.round(v + (to[i] - v) * k));
  return `rgb(${c[0]}, ${c[1]}, ${c[2]})`;
}

/** RuntimeManager.gpuLoad, asked twice a second: 0 while Nook leaves the GPU alone. */
function useGpuLoad(): number {
  const [load, setLoad] = useState(0);
  useEffect(() => {
    let alive = true;
    const poll = () =>
      runtimeGpuLoad()
        .then((l) => alive && setLoad(Number.isFinite(l) ? Math.max(0, Math.min(1, l)) : 0))
        .catch(() => alive && setLoad(0));
    poll();
    const timer = window.setInterval(poll, POLL_MS);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, []);
  return load;
}

export function FanMark({ size = 24, className }: { size?: number; className?: string }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const layer = useRef<HTMLCanvasElement | null>(null);
  const motor = useRef(new FanMotor());
  const blade = useRef<Path2D | null>(null);
  const load = useGpuLoad();
  const throttle = useRef(load);
  throttle.current = load;

  const draw = useCallback(() => {
    const c = canvas.current;
    const ctx = c?.getContext("2d");
    if (!c || !ctx) return;
    const px = Math.max(1, Math.round(size * (window.devicePixelRatio || 1)));
    if (c.width !== px) {
      c.width = px;
      c.height = px;
    }
    const css = getComputedStyle(document.documentElement);
    const tile = rgb(css.getPropertyValue("--fan-tile"), [0x44, 0x6d, 0x49]);
    const tileWarm = rgb(css.getPropertyValue("--fan-tile-warm"), [0x9d, 0x8f, 0xc4]);
    const bladeColour = css.getPropertyValue("--fan-blade").trim() || "#fffdf5";
    const m = motor.current;
    const unit = px / 24;
    const k = m.warmth;

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, px, px);
    ctx.fillStyle = warm(tile, tileWarm, k);
    ctx.beginPath();
    ctx.roundRect(0, 0, px, px, TILE_RADIUS * unit);
    ctx.fill();

    // Every frame lays the N down across the arc it swept in the last SHUTTER ms, heaviest at the
    // leading edge, so at speed the blades smear into a disc round a steady hub. The samples are
    // added up on a layer of their own, where they average to the blade's coverage.
    blade.current ??= new Path2D(N_PATH);
    const l = (layer.current ??= document.createElement("canvas"));
    if (l.width !== px) {
      l.width = px;
      l.height = px;
    }
    const lc = l.getContext("2d");
    if (!lc) return;
    lc.setTransform(1, 0, 0, 1, 0, 0);
    lc.clearRect(0, 0, px, px);
    lc.globalCompositeOperation = "lighter";
    lc.fillStyle = bladeColour;
    const arc = m.speed * SHUTTER;
    const n = Math.max(1, Math.min(64, Math.ceil(arc / 2)));
    let total = 0;
    for (let i = 0; i < n; i++) total += 1 - (0.6 * i) / n;
    const scale = 1 - 0.1 * k; // turning, the N draws in a little from the tile's edge, as the site's header mark does
    for (let i = 0; i < n; i++) {
      lc.setTransform(unit, 0, 0, unit, 0, 0);
      lc.translate(N_CENTRE.x, N_CENTRE.y);
      lc.rotate(((m.angle - (arc * i) / n) * Math.PI) / 180);
      lc.scale(scale, scale);
      lc.translate(-N_CENTRE.x, -N_CENTRE.y);
      lc.globalAlpha = (1 - (0.6 * i) / n) / total;
      lc.fill(blade.current);
    }
    ctx.drawImage(l, 0, 0);
  }, [size]);

  useEffect(() => draw(), [draw]);

  const spinning = load > 0;
  useEffect(() => {
    if (!spinning && motor.current.atRest) return;
    let raf = 0;
    let last = 0;
    const frame = (now: number) => {
      const dt = last === 0 || now <= last ? 16 : Math.min(40, now - last);
      last = now;
      motor.current.step(dt, throttle.current);
      draw();
      if (!motor.current.atRest) raf = requestAnimationFrame(frame);
    };
    raf = requestAnimationFrame(frame);
    return () => cancelAnimationFrame(raf);
  }, [spinning, draw]);

  return (
    <canvas
      ref={canvas}
      className={className}
      role="img"
      aria-label="Nook"
      style={{ width: size, height: size, display: "block", flex: "none" }}
    />
  );
}
