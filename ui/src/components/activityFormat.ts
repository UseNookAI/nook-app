/** Plain-language formatting shared by the settings pages (home/ActivityFormat.kt). */

/** "1,204" under ten thousand, then "48k", "1.2M". */
export function compact(n: number): string {
  if (n < 10_000) return Math.trunc(n).toLocaleString("en-US");
  if (n < 1_000_000) return `${Math.trunc(n / 1_000)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

/** "how long ago" for live cards and last-seen lines. `t` and `now` are Dates or epoch ms. */
export function ago(t: Date | number, now: Date | number = Date.now()): string {
  const ms = +now - +t;
  const minutes = Math.trunc(ms / 60_000);
  const hours = Math.trunc(ms / 3_600_000);
  const days = Math.trunc(ms / 86_400_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  if (hours < 24) return `${hours} h ago`;
  return `${days} d ago`;
}

const QUANT = /^(i?q\d.*|f16|f32|bf16|fp16)$/;
const SIZE = /^\d+(\.\d+)?b$/;

const trimChars = (s: string, chars: string) => {
  let a = 0;
  let b = s.length;
  while (a < b && chars.includes(s[a])) a++;
  while (b > a && chars.includes(s[b - 1])) b--;
  return s.slice(a, b);
};

/** "Qwen3 8B" from "qwen3-8b-q4km", "nomic-embed" from its catalog id, "Whisper small", "Z-Image Turbo". */
export function modelLabel(id: string | null | undefined): string {
  if (id == null || id.trim() === "") return "—";
  const lower = id.toLowerCase();
  if (lower.startsWith("nomic-embed")) return "nomic-embed";
  if (lower.startsWith("whisper")) {
    const rest = trimChars(lower.slice("whisper".length), "-.");
    const dash = rest.indexOf("-");
    return `Whisper ${dash >= 0 ? rest.slice(0, dash) : rest}`.trim();
  }
  if (lower.startsWith("z-image")) return "Z-Image Turbo";
  if (lower.startsWith("sdxl")) return "SDXL Turbo";
  if (lower.startsWith("sd-turbo") || lower.startsWith("sd_turbo")) return "SD Turbo";
  // Generic GGUF ids: drop the quantisation suffix, keep family and size.
  const parts: string[] = [];
  for (const part of lower.split("-")) {
    if (QUANT.test(part)) break;
    parts.push(part);
  }
  return parts
    .map((part) => {
      if (SIZE.test(part)) return part.toUpperCase();
      if (part.startsWith("v") && part.length > 1 && /\d/.test(part[1])) return part;
      return part.charAt(0).toUpperCase() + part.slice(1);
    })
    .join(" ")
    .trim();
}
