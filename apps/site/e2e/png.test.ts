// The blank-canvas assertion is only as good as the decoder under it, and the
// decoder is the one piece of the drive a browser is not needed to exercise.
// Every picture here is built in the test, so this needs no fixture and no LFS
// object: an encoder that applies each of the five scanline filters is the
// only way to cover the reconstruction they are reconstructed by.
import { deflateSync } from "node:zlib";

import { describe, expect, test } from "bun:test";

import { decodePng, fingerprint, hex, uniformity } from "./png.ts";

const WIDTH = 7;
const HEIGHT = 5;

/** `(x, y)` as a colour that repeats nowhere, so a shifted row shows up. */
function gradient(): Uint8Array {
  const pixels = new Uint8Array(WIDTH * HEIGHT * 4);
  for (let y = 0; y < HEIGHT; y++) {
    for (let x = 0; x < WIDTH; x++) {
      const at = (y * WIDTH + x) * 4;
      pixels[at] = x * 31;
      pixels[at + 1] = y * 47;
      pixels[at + 2] = (x * y * 13) & 0xff;
      pixels[at + 3] = 255;
    }
  }
  return pixels;
}

function flat(r: number, g: number, b: number): Uint8Array {
  const pixels = new Uint8Array(WIDTH * HEIGHT * 4);
  for (let at = 0; at < pixels.length; at += 4) {
    pixels.set([r, g, b, 255], at);
  }
  return pixels;
}

/**
 * An 8-bit RGBA PNG of `pixels`, every scanline filtered with `filter`.
 *
 * The chunk CRCs are zero: the decoder does not check them, and a real CRC
 * here would only be testing this function.
 */
function encode(pixels: Uint8Array, filter: number): Uint8Array {
  const stride = WIDTH * 4;
  const raw = new Uint8Array(HEIGHT * (stride + 1));
  for (let y = 0; y < HEIGHT; y++) {
    raw[y * (stride + 1)] = filter;
    for (let x = 0; x < stride; x++) {
      const a = x >= 4 ? pixels[y * stride + x - 4]! : 0;
      const b = y > 0 ? pixels[(y - 1) * stride + x]! : 0;
      const c = x >= 4 && y > 0 ? pixels[(y - 1) * stride + x - 4]! : 0;
      raw[y * (stride + 1) + 1 + x] = (pixels[y * stride + x]! - predict(filter, a, b, c)) & 0xff;
    }
  }

  const ihdr = new Uint8Array(13);
  new DataView(ihdr.buffer).setUint32(0, WIDTH);
  new DataView(ihdr.buffer).setUint32(4, HEIGHT);
  ihdr.set([8, 6, 0, 0, 0], 8);
  return concat([
    new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", new Uint8Array(deflateSync(raw))),
    chunk("IEND", new Uint8Array(0)),
  ]);
}

function chunk(kind: string, body: Uint8Array): Uint8Array {
  const out = new Uint8Array(12 + body.length);
  new DataView(out.buffer).setUint32(0, body.length);
  for (let at = 0; at < 4; at++) out[4 + at] = kind.charCodeAt(at);
  out.set(body, 8);
  return out;
}

function predict(filter: number, a: number, b: number, c: number): number {
  if (filter === 1) return a;
  if (filter === 2) return b;
  if (filter === 3) return (a + b) >> 1;
  if (filter === 4) {
    const p = a + b - c;
    const [da, db, dc] = [Math.abs(p - a), Math.abs(p - b), Math.abs(p - c)];
    return da <= db && da <= dc ? a : db <= dc ? b : c;
  }
  return 0;
}

function concat(parts: Uint8Array[]): Uint8Array {
  const all = new Uint8Array(parts.reduce((n, part) => n + part.length, 0));
  let at = 0;
  for (const part of parts) {
    all.set(part, at);
    at += part.length;
  }
  return all;
}

describe("decodePng", () => {
  for (const filter of [0, 1, 2, 3, 4]) {
    test(`reconstructs a picture filtered with ${filter}`, () => {
      const pixels = gradient();
      const image = decodePng(encode(pixels, filter));
      expect(image.width).toBe(WIDTH);
      expect(image.height).toBe(HEIGHT);
      expect(image.channels).toBe(4);
      expect([...image.data]).toEqual([...pixels]);
    });
  }

  test("refuses what it cannot read", () => {
    expect(() => decodePng(new Uint8Array(16))).toThrow("not a PNG");
  });
});

describe("uniformity", () => {
  test("a blank picture is one colour and all of it", () => {
    const image = decodePng(encode(flat(0x32, 0xa0, 0xaa), 0));
    const { colour, share } = uniformity(image);
    expect(share).toBe(1);
    expect(hex(colour, 4)).toBe("#32a0aaff");
  });

  test("one stray pixel is not enough to call a picture drawn", () => {
    const pixels = flat(0, 0, 0);
    pixels.set([255, 255, 255, 255], 0);
    expect(uniformity(decodePng(encode(pixels, 0))).share).toBeGreaterThan(0.95);
  });

  test("a drawn picture has no majority colour", () => {
    expect(uniformity(decodePng(encode(gradient(), 0))).share).toBeLessThan(0.5);
  });
});

describe("fingerprint", () => {
  test("one changed pixel changes it", () => {
    const pixels = gradient();
    const before = fingerprint(decodePng(encode(pixels, 0)));
    pixels[17] = pixels[17]! ^ 0xff;
    expect(fingerprint(decodePng(encode(pixels, 0)))).not.toBe(before);
  });
});
