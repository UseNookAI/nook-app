/** WaveformVisualizer.kt: the voice prompt's live waveform, and the round icon button beside it. */
import { useEffect, useRef } from "react";
import { Icon } from "../../components/Icon";
import "./code.css";

const TARGET_BAR_COUNT = 55;

/** Maps the raw levels to a symmetric row of bars, the most recent in the centre. */
function symmetric(amplitudes: number[], gain: number): number[] {
  const centerIndex = Math.floor(TARGET_BAR_COUNT / 2);
  return Array.from({ length: TARGET_BAR_COUNT }, (_, index) => {
    if (amplitudes.length === 0) return 0.1;
    const distance = Math.abs(index - centerIndex);
    const percentFromCenter = distance / centerIndex;
    // Map to source (center is most recent)
    const dataIndex = Math.trunc((amplitudes.length - 1) * (1 - percentFromCenter));
    const rawAmp = amplitudes[Math.min(Math.max(dataIndex, 0), amplitudes.length - 1)];
    // Boost the signal so quiet sounds are visible; keep the middle tall, drop off the edges.
    const boostedAmp = Math.min(Math.max(rawAmp * gain, 0.1), 1.0);
    const falloff = Math.min(Math.max(1 - percentFromCenter, 0.2), 1.0);
    return boostedAmp * falloff;
  });
}

/**
 * Bars that follow the microphone level. [barWidth] is in device pixels, as Compose's Canvas drew
 * it, and each bar eases to its new height (a stiff spring with no bounce there).
 */
export function WaveformVisualizer({
  amplitudes,
  width = 120,
  height = 24,
  barColor = "var(--text-secondary)",
  barWidth = 4,
  gain = 2.0,
}: {
  amplitudes: number[];
  width?: number;
  height?: number;
  barColor?: string;
  barWidth?: number;
  gain?: number;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const target = useRef<number[]>(symmetric([], gain));
  const shown = useRef<number[]>(symmetric([], gain));

  useEffect(() => {
    target.current = symmetric(amplitudes, gain);
  }, [amplitudes, gain]);

  useEffect(() => {
    let frame = 0;
    let last = performance.now();
    const draw = (now: number) => {
      const el = canvas.current;
      if (el) {
        const dpr = window.devicePixelRatio || 1;
        const w = Math.round(width * dpr);
        const h = Math.round(height * dpr);
        if (el.width !== w) el.width = w;
        if (el.height !== h) el.height = h;
        const dt = Math.min(100, now - last);
        const k = 1 - Math.exp(-dt / 45);
        shown.current = shown.current.map((v, i) => v + (target.current[i] - v) * k);
        const ctx = el.getContext("2d");
        if (ctx) {
          ctx.clearRect(0, 0, w, h);
          ctx.fillStyle = getComputedStyle(el).color;
          const totalBars = shown.current.length;
          const dynamicGap = totalBars > 1 ? (w - totalBars * barWidth) / (totalBars - 1) : 0;
          shown.current.forEach((amp, index) => {
            const x = index * (barWidth + dynamicGap);
            // Even "quiet" bars keep a visible minimum height.
            const barHeight = Math.max(h * amp, 8);
            const y = (h - barHeight) / 2;
            ctx.beginPath();
            ctx.roundRect(x, y, barWidth, barHeight, barWidth / 2);
            ctx.fill();
          });
        }
      }
      last = now;
      frame = requestAnimationFrame(draw);
    };
    frame = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(frame);
  }, [width, height, barWidth]);

  return <canvas ref={canvas} style={{ width, height, color: barColor, flex: "none" }} aria-hidden />;
}

/** A 36 px round icon button with a hover disc (PromptIconButton). */
export function PromptIconButton({
  onClick,
  icon,
  label,
  hoverBackground,
  tint,
  enabled = true,
  iconSize = 16,
}: {
  onClick: () => void;
  icon: string;
  label: string;
  hoverBackground: string;
  tint: string;
  enabled?: boolean;
  iconSize?: number;
}) {
  return (
    <button
      type="button"
      className="nc-prompt-icon"
      disabled={!enabled}
      onClick={onClick}
      title={label}
      aria-label={label}
      style={{ ["--nc-hover" as string]: hoverBackground, color: tint }}
    >
      <Icon name={icon} size={iconSize} />
    </button>
  );
}
