// The blank-canvas assertion is only as good as the two judgements under it,
// and they are the one piece of the drive a browser is not needed to exercise.
// Every picture here is built in the test, so this needs no fixture and no LFS
// object.
import { describe, expect, test } from "bun:test";

import { fingerprint, hex, uniformity } from "./frame.ts";
import type { Image } from "./frame.ts";

const WIDTH = 7;
const HEIGHT = 5;

function image(data: Uint8Array): Image {
  return { width: WIDTH, height: HEIGHT, channels: 4, data };
}

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

describe("uniformity", () => {
  test("a blank picture is one colour and all of it", () => {
    const { colour, share } = uniformity(image(flat(0x32, 0xa0, 0xaa)));
    expect(share).toBe(1);
    expect(hex(colour, 4)).toBe("#32a0aaff");
  });

  test("one stray pixel is not enough to call a picture drawn", () => {
    const pixels = flat(0, 0, 0);
    pixels.set([255, 255, 255, 255], 0);
    expect(uniformity(image(pixels)).share).toBeGreaterThan(0.95);
  });

  test("a drawn picture has no majority colour", () => {
    expect(uniformity(image(gradient())).share).toBeLessThan(0.5);
  });

  test("an empty picture reads as blank rather than dividing by zero", () => {
    expect(uniformity({ width: 0, height: 0, channels: 4, data: new Uint8Array(0) })).toEqual({
      colour: 0,
      share: 1,
    });
  });
});

describe("fingerprint", () => {
  test("one changed pixel changes it", () => {
    const pixels = gradient();
    const before = fingerprint(image(pixels));
    pixels[17] = pixels[17]! ^ 0xff;
    expect(fingerprint(image(pixels))).not.toBe(before);
  });
});
