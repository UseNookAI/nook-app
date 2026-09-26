/** ContextMeter.kt: how full the worker's context is, beside the composer. */
import type { NextContext, RunContext } from "../../api/code";
import { Tooltip } from "../../components/Tooltip";
import "./code.css";

/**
 * What the context meter shows: [used] of the [window] tokens one request has, [estimated] until
 * the engine has counted them, and what its tooltip says.
 */
export interface ContextReading {
  used: number;
  window: number;
  estimated: boolean;
  detail: string;
}

const fractionOf = (r: ContextReading) => (r.window <= 0 ? 0 : Math.min(1, Math.max(0, r.used / r.window)));
const percentOf = (r: ContextReading) => (r.window <= 0 ? 0 : Math.min(100, Math.max(0, Math.floor((r.used * 100) / r.window))));

function contextTint(fraction: number): string {
  if (fraction >= 0.9) return "var(--error)";
  if (fraction >= 0.7) return "var(--warning)";
  return "var(--text-secondary)";
}

/**
 * How full the worker's context is, beside the composer: a ring that fills with it, amber from
 * 70% and red from 90%, and the percentage; "~" while it is an estimate. The tooltip says what it
 * holds.
 */
export function ContextMeter({ reading }: { reading: ContextReading }) {
  const fraction = fractionOf(reading);
  const tint = contextTint(fraction);
  // A 14 px ring with a 2 px stroke.
  const r = 6;
  const c = 2 * Math.PI * r;
  return (
    <Tooltip text={reading.detail} placement="top" delay={300} maxWidth={340} roomy>
      <span className="nc-context-meter">
        <svg width={14} height={14} viewBox="0 0 14 14" aria-hidden>
          <circle cx={7} cy={7} r={r} fill="none" stroke="var(--border)" strokeWidth={2} />
          {fraction > 0 && (
            <circle
              cx={7}
              cy={7}
              r={r}
              fill="none"
              stroke={tint}
              strokeWidth={2}
              strokeLinecap="round"
              strokeDasharray={`${c * fraction} ${c}`}
              transform="rotate(-90 7 7)"
            />
          )}
        </svg>
        <span className="caption" style={{ color: fraction >= 0.7 ? tint : "var(--text-tertiary)" }}>
          {(reading.estimated ? "~" : "") + `${percentOf(reading)}%`}
        </span>
      </span>
    </Tooltip>
  );
}

const tokens = (n: number) => n.toLocaleString("en-US");

/** While a run works: how full its context was after the model's last reply. */
export function liveReading(c: RunContext): ContextReading {
  let detail = `Context: ${tokens(c.used)} of ${tokens(c.window)} tokens`;
  detail += c.measured ? ", as the model counted them after its last reply." : ", estimated.";
  detail += "\n\nThis request holds the instructions, your request with the earlier ones, and what the worker has read and run. ";
  detail += "Near the limit Nook drops the oldest file reads and command output";
  if (c.dropped > 0) detail += ` (${c.dropped} so far)`;
  detail += ", which the worker can read again.";
  return { used: c.used, window: c.window, estimated: !c.measured, detail };
}

/** Between requests: what the next one starts with, before the worker reads anything. */
export function nextReading(n: NextContext): ContextReading {
  let detail = `About ${tokens(n.tokens)} of ${tokens(n.window)} tokens before the worker reads anything: `;
  detail += "the instructions and tools";
  if (n.recap > 0) detail += `, and your earlier requests with what the worker did (about ${tokens(n.recap)})`;
  detail += ".\n\nEach request starts afresh with these, and the rest is room for reading files and running commands. ";
  detail += "A long session carries more, so start a new one when this gets high.";
  detail += `\n\nOne request holds at most ${tokens(n.window)} tokens: `;
  detail += "what Nook gives this model, so it fits in the GPU's memory.";
  return { used: n.tokens, window: n.window, estimated: true, detail };
}

/** "context up to 71%, 3 outputs dropped", for a finished run's line. */
export function peakLine(c: RunContext): string {
  const pct = c.window <= 0 ? 0 : Math.min(100, Math.max(0, Math.floor((c.peak * 100) / c.window)));
  const outputs = c.dropped === 1 ? "output" : "outputs";
  const dropped = c.dropped > 0 ? `, ${c.dropped} ${outputs} dropped` : "";
  return `context up to ${c.measured ? "" : "~"}${pct}%${dropped}`;
}
