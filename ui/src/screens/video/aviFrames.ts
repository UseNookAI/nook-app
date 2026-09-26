/**
 * AviFrames.kt: the frames of an MJPEG AVI as the video engine writes it: every frame one JPEG in
 * a `00dc` chunk of the `movi` list, played at the rate the `avih` header gives. The webview has
 * no AVI player; with this the page plays a clip by decoding one JPEG per frame.
 */

/** The frame rate when the header has none: the Wan models' own. */
export const DEFAULT_FPS = 16;

export interface AviFrames {
  readonly fps: number;
  readonly width: number;
  readonly height: number;
  readonly count: number;
  /** The encoded JPEG of frame `index` (a view into the file's bytes, not a copy). */
  jpeg(index: number): Uint8Array<ArrayBuffer>;
}

/** Parses an AVI file's bytes. Throws "Not an AVI file." or "The AVI file has no video frames.". */
export function parseAvi(buffer: ArrayBuffer | Uint8Array<ArrayBuffer>): AviFrames {
  const data = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  const fourcc = (at: number) => String.fromCharCode(data[at], data[at + 1], data[at + 2], data[at + 3]);
  const u32 = (at: number) => view.getUint32(at, true);

  if (!(data.length >= 12 && fourcc(0) === "RIFF" && fourcc(8) === "AVI ")) throw new Error("Not an AVI file.");
  let microsPerFrame = 0;
  let width = 0;
  let height = 0;
  const offsets: number[] = [];
  const lengths: number[] = [];

  const walk = (start: number, end: number) => {
    let p = start;
    while (p + 8 <= end) {
      const id = fourcc(p);
      const size = u32(p + 4);
      const body = p + 8;
      const bodyEnd = Math.min(end, body + size);
      if (id === "LIST") {
        if (bodyEnd - body >= 4) walk(body + 4, bodyEnd);
      } else if (id === "avih" && bodyEnd - body >= 40) {
        microsPerFrame = u32(body);
        width = u32(body + 32);
        height = u32(body + 36);
      } else if ((id === "00dc" || id === "00db") && bodyEnd > body) {
        // Video stream 00: compressed (dc) or uncompressed (db) frames; audio is 01wb.
        offsets.push(body);
        lengths.push(bodyEnd - body);
      }
      // Chunks are padded to an even length.
      const next = body + size + (size % 2);
      if (next > end) break;
      p = next;
    }
  };

  const riffEnd = Math.min(data.length, 8 + u32(4));
  walk(12, riffEnd);
  if (offsets.length === 0) throw new Error("The AVI file has no video frames.");
  const fps = microsPerFrame > 0 ? 1_000_000 / microsPerFrame : DEFAULT_FPS;
  return {
    fps,
    width,
    height,
    count: offsets.length,
    jpeg: (index: number) => data.subarray(offsets[index], offsets[index] + lengths[index]),
  };
}
