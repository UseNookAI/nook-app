/**
 * The greeting's icon (TimeOfDayIcon.kt): the sun or the moon for `part`, drawn like the app's
 * other icons (a 24-unit box, 1.75 lines, round ends) and kept just alive. The sun's rays breathe
 * and turn about once in 40 seconds, a sunrise lifts a little off the horizon and a sunset sinks,
 * and at night the moon rocks while two stars twinkle. Amber for the sun and lavender for the
 * evening, from the theme.
 */
import { useEffect, useId, useState } from "react";
import type { DayPart } from "./dayPart";

/** The moon of icons/moon.svg, in the same 24-unit box. */
const MOON_PATH = "M20.5 14.2A8.5 8.5 0 1 1 9.8 3.5a9 9 0 0 0 10.7 10.7Z";
const LINE = 1.75;
const HORIZON = 16.5;

/** Motion this slow looks the same at 30 frames a second, and costs half of 60. */
const FRAME_MS = 33;

const SUN = "var(--warning)";
const SUN_GLOW = "var(--warning-soft)";
const DUSK = "var(--secondary)";
const DUSK_GLOW = "var(--secondary-soft)";

/** 0 to 1 and back, smoothly, once every `period` seconds. */
const wave = (t: number, period: number) => 0.5 - 0.5 * Math.cos((2 * Math.PI * t) / period);

function useSeconds(): number {
  const [t, setT] = useState(0);
  useEffect(() => {
    const start = performance.now();
    const timer = window.setInterval(() => setT((performance.now() - start) / 1000), FRAME_MS);
    return () => window.clearInterval(timer);
  }, []);
  return t;
}

function Ray({ cx, cy, degrees, from, to, color }: { cx: number; cy: number; degrees: number; from: number; to: number; color: string }) {
  const a = (degrees * Math.PI) / 180;
  const dx = Math.cos(a);
  const dy = Math.sin(a);
  return <line x1={cx + dx * from} y1={cy + dy * from} x2={cx + dx * to} y2={cy + dy * to} stroke={color} strokeWidth={LINE} strokeLinecap="round" />;
}

/** The afternoon sun: eight rays, every other one breathing out of step, turning slowly. */
function FullSun({ t }: { t: number }) {
  return (
    <>
      {Array.from({ length: 8 }, (_, i) => (
        <Ray key={i} cx={12} cy={12} degrees={i * 45 + t * 9} from={6.75} to={8.7 + 0.8 * wave(t + (i % 2) * 1.8, 3.6)} color={SUN} />
      ))}
      <circle cx={12} cy={12} r={4.25} fill={SUN_GLOW} stroke={SUN} strokeWidth={LINE} />
    </>
  );
}

/** The sun on the horizon, lifting off it a little (`rising`) or settling below it. */
function HorizonSun({ t, rising, ground, clipId }: { t: number; rising: boolean; ground: string; clipId: string }) {
  const drift = wave(t, 5);
  const cy = rising ? HORIZON - 1.2 * drift : HORIZON + 0.9 * drift;
  return (
    <>
      <clipPath id={clipId}>
        <rect x={-4} y={-4} width={32} height={HORIZON - 1.4 + 4} />
      </clipPath>
      <g clipPath={`url(#${clipId})`}>
        {Array.from({ length: 5 }, (_, i) => (
          <Ray key={i} cx={12} cy={cy} degrees={-157.5 + i * 33.75} from={6.9} to={8.4 + 0.6 * wave(t + i * 0.7, 3.6)} color={SUN} />
        ))}
        <circle cx={12} cy={cy} r={4.6} fill={SUN_GLOW} stroke={SUN} strokeWidth={LINE} />
      </g>
      <line x1={2.5} y1={HORIZON} x2={21.5} y2={HORIZON} stroke={ground} strokeWidth={LINE} strokeLinecap="round" />
      <line x1={7.5} y1={20} x2={16.5} y2={20} stroke={ground} strokeWidth={LINE} strokeLinecap="round" />
    </>
  );
}

/** A four-pointed star that swells and fades, `phase` seconds out of step with the others. */
function Star({ t, x, y, r, color, phase }: { t: number; x: number; y: number; r: number; color: string; phase: number }) {
  const k = wave(t + phase, 2.8);
  const w = r * 0.16;
  const d =
    `M${x} ${y - r}Q${x + w} ${y - w} ${x + r} ${y}Q${x + w} ${y + w} ${x} ${y + r}` +
    `Q${x - w} ${y + w} ${x - r} ${y}Q${x - w} ${y - w} ${x} ${y - r}Z`;
  const s = 0.6 + 0.4 * k;
  return (
    <path
      d={d}
      fill={color}
      opacity={0.45 + 0.55 * k}
      transform={`translate(${x} ${y}) scale(${s}) translate(${-x} ${-y})`}
    />
  );
}

export function TimeOfDayIcon({ part, size = 32 }: { part: DayPart; size?: number }) {
  const t = useSeconds();
  const clipId = `nk-horizon-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" aria-hidden style={{ flex: "none", overflow: "visible" }}>
      {part === "DAY" && <FullSun t={t} />}
      {part === "SUNRISE" && <HorizonSun t={t} rising ground={SUN} clipId={clipId} />}
      {part === "SUNSET" && (
        <>
          <HorizonSun t={t} rising={false} ground={DUSK} clipId={clipId} />
          <Star t={t} x={19.5} y={4.75} r={1.9} color={DUSK} phase={0} />
        </>
      )}
      {part === "NIGHT" && (
        <>
          <g transform={`rotate(${4 * Math.sin((2 * Math.PI * t) / 7)} 12 12)`}>
            <path d={MOON_PATH} fill={DUSK_GLOW} stroke={DUSK} strokeWidth={LINE} strokeLinecap="round" strokeLinejoin="round" />
          </g>
          <Star t={t} x={16.6} y={7.4} r={2.3} color={DUSK} phase={0} />
          <Star t={t} x={20.6} y={3.9} r={1.4} color={DUSK} phase={1.3} />
        </>
      )}
    </svg>
  );
}
