/**
 * Scout, the Nooklet who finds the others: a mint bug in the Nooklets' flat style (antennae, pill
 * eyes, thin limbs; nook-promo/nooklets/bugs-cast.js), a magnifying glass held to one eye. It
 * breathes and blinks while `idle`, scans from side to side while `searching`, hops and waves
 * when it has `found` the Nooklet, and tilts its head at a question mark when it is `lost`.
 *
 * Every part that moves turns about its own joint: the joint is moved to the origin by an outer
 * <g>, the CSS animation (nooklets.css) turns the inner one.
 */
export type Mood = "idle" | "searching" | "found" | "lost";

const K = "#1B1A18";
const BODY = "#6CCFB2";
const DARK = "#43A68A";
const GLASS = "#4A4843";

function Antenna({ side }: { side: -1 | 1 }) {
  return (
    <g transform={`translate(${side < 0 ? -22 : 20} -220)`}>
      <g className={side < 0 ? "nlc-antenna nlc-antenna--left" : "nlc-antenna nlc-antenna--right"}>
        <path d={`M 0 0 C ${-4 * -side} -22 ${12 * side} -36 ${26 * side} -44`} fill="none" stroke={DARK} strokeWidth={6} strokeLinecap="round" />
        <circle cx={26 * side} cy={-44} r={9.5} fill={DARK} />
        <circle cx={26 * side - 3} cy={-47} r={3} fill="#FFFFFF" opacity={0.45} />
      </g>
    </g>
  );
}

/** A pill eye at (x, y); `w` by `h`. Happy: an upturned arc instead. */
function Eye({ x, y, w, h }: { x: number; y: number; w: number; h: number }) {
  return (
    <g transform={`translate(${x} ${y})`}>
      <g className="nlc-eye">
        <rect x={-w / 2} y={-h / 2} width={w} height={h} rx={w / 2} fill={K} />
        <rect x={-w / 2 + 3.5} y={-h / 2 + 4} width={w * 0.36} height={h * 0.3} rx={w * 0.18} fill="#FFFFFF" />
      </g>
      <path className="nlc-happy" d={`M ${-w * 0.65} ${h * 0.1} Q 0 ${-h * 0.45} ${w * 0.65} ${h * 0.1}`} fill="none" stroke={K} strokeWidth={6} strokeLinecap="round" />
    </g>
  );
}

function Sparkle({ x, y, s, delay }: { x: number; y: number; s: number; delay: number }) {
  return (
    <g transform={`translate(${x} ${y}) scale(${s})`}>
      <path className="nlc-sparkle" style={{ animationDelay: `${delay}ms` }} d="M 0 -14 Q 2 -2 14 0 Q 2 2 0 14 Q -2 2 -14 0 Q -2 -2 0 -14 Z" fill="#F7C548" />
    </g>
  );
}

export function Scout({ mood, size = 150 }: { mood: Mood; size?: number }) {
  return (
    <svg
      className={`nlc nlc--${mood}`}
      viewBox="-150 -312 300 330"
      width={(size * 300) / 330}
      height={size}
      role="img"
      aria-label={
        mood === "searching"
          ? "Scout is looking for the right Nooklet"
          : mood === "found"
            ? "Scout found a Nooklet"
            : mood === "lost"
              ? "Scout found nothing that fits"
              : "Scout, who finds the right Nooklet"
      }
    >
      <ellipse className="nlc-shadow" cx={0} cy={4} rx={66} ry={9} fill="rgba(27,26,24,0.10)" />
      <g className="nlc-all">
        {/* The legs, under the body so their tops stay hidden as it breathes. */}
        <path d="M -28 -104 Q -34 -52 -31 -8" fill="none" stroke={DARK} strokeWidth={11} strokeLinecap="round" />
        <ellipse cx={-38} cy={-5} rx={14} ry={7} fill={DARK} />
        <path d="M 24 -104 Q 30 -52 27 -8" fill="none" stroke={DARK} strokeWidth={11} strokeLinecap="round" />
        <ellipse cx={34} cy={-5} rx={14} ry={7} fill={DARK} />

        <g transform="translate(0 -96)">
          <g className="nlc-upper">
            <g transform="translate(0 96)">
              <g className="nlc-breathe">
                <rect x={-64} y={-222} width={150} height={130} rx={46} fill={DARK} />
                <rect x={-76} y={-224} width={150} height={130} rx={46} fill={BODY} />
                <rect x={-58} y={-212} width={26} height={12} rx={6} fill="#FFFFFF" opacity={0.35} />
                <Antenna side={-1} />
                <Antenna side={1} />

                <ellipse cx={-47} cy={-140} rx={11} ry={6.6} fill="#FF6F8E" opacity={0.4} />
                <ellipse cx={58} cy={-128} rx={9} ry={5.4} fill="#FF6F8E" opacity={0.4} />

                <g className="nlc-look">
                  <Eye x={-30} y={-168} w={17} h={30} />
                  {/* The eye behind the glass, magnified. */}
                  <Eye x={26} y={-168} w={24} h={40} />
                </g>

                {/* The mouth: a smile, an "o" while searching, a wobble when lost. */}
                <path className="nlc-mouth nlc-mouth--smile" d="M -20 -136 Q -10 -127 0 -136" fill="none" stroke={K} strokeWidth={5} strokeLinecap="round" />
                <ellipse className="nlc-mouth nlc-mouth--o" cx={-10} cy={-133} rx={5.5} ry={6.5} fill="#3A1F2B" />
                <path className="nlc-mouth nlc-mouth--open" d="M -22 -137 Q -10 -118 2 -137 Z" fill="#3A1F2B" />
                <path className="nlc-mouth nlc-mouth--wobble" d="M -21 -133 q 5 -4 10 0 t 10 0" fill="none" stroke={K} strokeWidth={5} strokeLinecap="round" />

                {/* The magnifying glass, held to the right eye. */}
                <line x1={45} y1={-149} x2={66} y2={-126} stroke={GLASS} strokeWidth={8} strokeLinecap="round" />
                <circle cx={26} cy={-168} r={27} fill="#FFFFFF" fillOpacity={0.22} stroke={GLASS} strokeWidth={7} />
                <path d="M 8 -180 A 20 20 0 0 1 20 -190" fill="none" stroke="#FFFFFF" strokeWidth={4} strokeLinecap="round" opacity={0.8} />
                <path d="M 74 -108 Q 90 -112 68 -126" fill="none" stroke={DARK} strokeWidth={11} strokeLinecap="round" />
                <circle cx={67} cy={-126} r={8} fill={DARK} />

                {/* The free arm: at its side, waving when found. */}
                <g transform="translate(-74 -152)">
                  <g className="nlc-wave">
                    <path d="M 0 0 Q -24 14 -20 38" fill="none" stroke={DARK} strokeWidth={11} strokeLinecap="round" />
                    <circle cx={-20} cy={38} r={8} fill={DARK} />
                  </g>
                </g>
              </g>
            </g>
          </g>
        </g>
      </g>

      <g className="nlc-dots" fill={DARK} aria-hidden>
        <circle cx={-18} cy={-292} r={6} />
        <circle cx={0} cy={-292} r={6} />
        <circle cx={18} cy={-292} r={6} />
      </g>
      <g className="nlc-sparkles" aria-hidden>
        <Sparkle x={-92} y={-258} s={0.9} delay={80} />
        <Sparkle x={96} y={-270} s={1.1} delay={0} />
        <Sparkle x={112} y={-214} s={0.6} delay={160} />
      </g>
      <text className="nlc-question" x={84} y={-252} textAnchor="middle" fill={DARK} aria-hidden>
        ?
      </text>
    </svg>
  );
}
