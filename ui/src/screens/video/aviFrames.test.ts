// AviFramesTest.kt, ported, plus the audio and truncation cases the engine's files can have.
import { describe, expect, it } from "vitest";
import { DEFAULT_FPS, parseAvi } from "./aviFrames";

const ascii = (s: string): Uint8Array<ArrayBuffer> => Uint8Array.from(s, (c) => c.charCodeAt(0));

function concat(...parts: Uint8Array[]): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

function u32(...values: number[]): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(values.length * 4);
  const view = new DataView(out.buffer);
  values.forEach((v, i) => view.setUint32(i * 4, v, true));
  return out;
}

function chunk(id: string, body: Uint8Array): Uint8Array<ArrayBuffer> {
  return concat(ascii(id), u32(body.length), body, body.length % 2 === 1 ? new Uint8Array(1) : new Uint8Array(0));
}

const list = (type: string, ...children: Uint8Array[]) => chunk("LIST", concat(ascii(type), ...children));

/** An AVI laid out as the engine writes one: header list, frames in movi, then the index. */
function avi(microsPerFrame: number, frames: Uint8Array[], extra: Uint8Array[] = []): Uint8Array<ArrayBuffer> {
  // avih: the rate first, the width at offset 32 and the height at 36.
  const avih = u32(microsPerFrame, 0, 0, 0, 0, 0, 0, 0, 832, 480, 0, 0, 0, 0);
  const hdrl = list("hdrl", chunk("avih", avih), list("strl", chunk("strh", new Uint8Array(56))));
  const movi = list("movi", ...frames.map((f) => chunk("00dc", f)), ...extra);
  // The index names every 00dc again; the reader must not count those as frames.
  const idx1 = chunk("idx1", concat(ascii("00dc"), new Uint8Array(12), ascii("00dc"), new Uint8Array(12)));
  const body = concat(ascii("AVI "), hdrl, movi, idx1);
  return concat(ascii("RIFF"), u32(body.length), body);
}

describe("parseAvi", () => {
  it("reads every frame and the rate", () => {
    const a = Uint8Array.of(1, 2, 3); // odd length: padded in the file, not in the frame
    const b = Uint8Array.of(4, 5, 6, 7);
    const frames = parseAvi(avi(62_500, [a, b]));
    expect(frames.count).toBe(2);
    expect(frames.fps).toBe(16);
    expect(frames.width).toBe(832);
    expect(frames.height).toBe(480);
    expect(Array.from(frames.jpeg(0))).toEqual([1, 2, 3]);
    expect(Array.from(frames.jpeg(1))).toEqual([4, 5, 6, 7]);
  });

  it("falls back to the models' own rate when the header has none", () => {
    expect(parseAvi(avi(0, [Uint8Array.of(9)])).fps).toBe(DEFAULT_FPS);
  });

  it("refuses something else", () => {
    expect(() => parseAvi(ascii("not a video at all"))).toThrow("Not an AVI file.");
    expect(() => parseAvi(avi(62_500, []))).toThrow("The AVI file has no video frames.");
  });

  it("skips audio and takes uncompressed frames", () => {
    const frames = parseAvi(avi(40_000, [Uint8Array.of(1)], [chunk("01wb", Uint8Array.of(7, 7)), chunk("00db", Uint8Array.of(2, 2))]));
    expect(frames.fps).toBe(25);
    expect(frames.count).toBe(2);
    expect(Array.from(frames.jpeg(1))).toEqual([2, 2]);
  });

  it("reads the frames that are there when the file was cut short", () => {
    const second = Uint8Array.of(0xab, 0xcd, 0xef, 0x12, 0x34, 0x56);
    const whole = avi(62_500, [Uint8Array.of(1, 1), second]);
    const at = whole.findIndex((_, i) => whole[i] === 0xab && whole[i + 1] === 0xcd && whole[i + 2] === 0xef);
    // Cut inside the second frame: it keeps what was written, and the missing index is no matter.
    const frames = parseAvi(whole.slice(0, at + 3).buffer);
    expect(frames.count).toBe(2);
    expect(Array.from(frames.jpeg(1))).toEqual([0xab, 0xcd, 0xef]);
  });

  it("reads from a view into a larger buffer", () => {
    const file = avi(62_500, [Uint8Array.of(5, 6)]);
    const padded = new Uint8Array(file.length + 10);
    padded.set(file, 10);
    expect(Array.from(parseAvi(padded.subarray(10)).jpeg(0))).toEqual([5, 6]);
  });
});
