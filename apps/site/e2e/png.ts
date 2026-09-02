/**
 * Just enough PNG to tell a drawn frame from a blank one.
 *
 * The drive's one question about a canvas is "did anything reach it?", and the
 * page cannot answer that. A WebGL2 context without `preserveDrawingBuffer`
 * has an undefined drawing buffer once the frame is presented, so `toDataURL`,
 * `readPixels` and a `drawImage` into a 2D canvas all read back empty on a
 * canvas that is visibly drawing. The compositor's copy is the honest one, and
 * Playwright hands that over as a PNG — which someone then has to decode.
 *
 * A decoding dependency is more than this needs. Playwright writes 8-bit
 * non-interlaced images, so the format here is an IHDR, the concatenated IDAT
 * stream through `node:zlib`, and the five scanline filters: about sixty lines
 * against a package, its version and its own transitive tree.
 */

import { inflateSync } from "node:zlib";

const SIGNATURE = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

/** Channels per pixel, by PNG colour type. */
const CHANNELS: Record<number, number> = { 0: 1, 2: 3, 3: 1, 4: 2, 6: 4 };

export interface Image {
  width: number;
  height: number;
  /** Bytes per pixel: 4 for the RGBA8 a screenshot is. */
  channels: number;
  /** Tightly packed pixels, row-major, `channels` bytes each. */
  data: Uint8Array;
}

/** The picture in `bytes`, or a throw naming what about it is unsupported. */
export function decodePng(bytes: Uint8Array): Image {
  for (let at = 0; at < SIGNATURE.length; at++) {
    if (bytes[at] !== SIGNATURE[at]) throw new Error("not a PNG");
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let width = 0;
  let height = 0;
  let channels = 0;
  const compressed: Uint8Array[] = [];
  for (let at = 8; at + 8 <= bytes.length; ) {
    const length = view.getUint32(at);
    const kind = String.fromCharCode(...bytes.subarray(at + 4, at + 8));
    const body = at + 8;
    if (kind === "IHDR") {
      width = view.getUint32(body);
      height = view.getUint32(body + 4);
      const depth = bytes[body + 8] ?? 0;
      const colour = bytes[body + 9] ?? 0;
      const interlace = bytes[body + 12] ?? 0;
      if (depth !== 8) throw new Error(`unsupported PNG: ${depth} bits per channel`);
      if (interlace !== 0) throw new Error("unsupported PNG: interlaced");
      if (colour === 3) throw new Error("unsupported PNG: palette");
      channels = CHANNELS[colour] ?? 0;
      if (channels === 0) throw new Error(`unsupported PNG: colour type ${colour}`);
    } else if (kind === "IDAT") {
      compressed.push(bytes.subarray(body, body + length));
    } else if (kind === "IEND") {
      break;
    }
    at = body + length + 4;
  }
  if (width === 0 || height === 0) throw new Error("PNG carries no IHDR");

  const raw = new Uint8Array(inflateSync(join(compressed)));
  const stride = width * channels;
  if (raw.length < height * (stride + 1)) throw new Error("PNG is short of pixel data");

  // Every scanline is prefixed by its filter type and predicted from the pixel
  // to its left (a), the one above (b) and the one above-left (c); the output
  // rows are the reconstruction, so a row is unfiltered against rows already
  // reconstructed rather than against the raw bytes.
  const data = new Uint8Array(height * stride);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)] ?? 0;
    const from = y * (stride + 1) + 1;
    const row = y * stride;
    const above = row - stride;
    // Every index below is inside the two arrays: the ternaries hold the
    // neighbours to the picture, and the length check above holds `raw`.
    for (let x = 0; x < stride; x++) {
      const a = x >= channels ? data[row + x - channels]! : 0;
      const b = y > 0 ? data[above + x]! : 0;
      const c = x >= channels && y > 0 ? data[above + x - channels]! : 0;
      data[row + x] = (raw[from + x]! + predict(filter, a, b, c)) & 0xff;
    }
  }
  return { width, height, channels, data };
}

export interface Uniformity {
  /** The colour, packed most-significant channel first. */
  colour: number;
  /** Its share of the picture, 0 to 1. */
  share: number;
}

/**
 * The colour covering most of a picture, whenever one covers more than half.
 *
 * Blank is the failure being hunted and blank means "one colour", so the
 * number worth being exact about is a share near 1. Boyer-Moore's majority
 * vote finds in one pass, and no memory, the only value that can hold a
 * majority; a second pass counts it exactly. A picture with no majority colour
 * is already not blank, and simply reports a share below a half — which is why
 * this beats a histogram that would allocate a map entry per distinct colour
 * of a megapixel screenshot.
 */
export function uniformity(image: Image): Uniformity {
  const { data, channels } = image;
  const total = data.length / channels;
  if (total === 0) return { colour: 0, share: 1 };

  let candidate = 0;
  let votes = 0;
  for (let at = 0; at < data.length; at += channels) {
    const pixel = pack(data, at, channels);
    if (votes === 0) {
      candidate = pixel;
      votes = 1;
    } else if (pixel === candidate) {
      votes++;
    } else {
      votes--;
    }
  }

  let count = 0;
  for (let at = 0; at < data.length; at += channels) {
    if (pack(data, at, channels) === candidate) count++;
  }
  return { colour: candidate, share: count / total };
}

/** FNV-1a over the decoded pixels: two frames differ iff this differs. */
export function fingerprint(image: Image): number {
  let hash = 0x811c9dc5;
  for (const byte of image.data) {
    hash = Math.imul(hash ^ byte, 0x01000193);
  }
  return hash >>> 0;
}

/** A packed colour as `#rrggbbaa`, for an error message. */
export function hex(colour: number, channels: number): string {
  return `#${colour.toString(16).padStart(channels * 2, "0")}`;
}

function pack(data: Uint8Array, at: number, channels: number): number {
  let key = 0;
  for (let c = 0; c < channels; c++) key = (key * 256 + data[at + c]!) >>> 0;
  return key;
}

function predict(filter: number, a: number, b: number, c: number): number {
  switch (filter) {
    case 0:
      return 0;
    case 1:
      return a;
    case 2:
      return b;
    case 3:
      return (a + b) >> 1;
    case 4: {
      const p = a + b - c;
      const da = Math.abs(p - a);
      const db = Math.abs(p - b);
      const dc = Math.abs(p - c);
      return da <= db && da <= dc ? a : db <= dc ? b : c;
    }
    default:
      throw new Error(`unknown PNG scanline filter ${filter}`);
  }
}

function join(parts: Uint8Array[]): Uint8Array {
  const all = new Uint8Array(parts.reduce((n, part) => n + part.length, 0));
  let at = 0;
  for (const part of parts) {
    all.set(part, at);
    at += part.length;
  }
  return all;
}
