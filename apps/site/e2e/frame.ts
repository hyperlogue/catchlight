/**
 * Just enough pixel arithmetic to tell a drawn frame from a blank one.
 *
 * The drive's one question about a canvas is "did anything reach it?", and in
 * this browser only the renderer can answer it. A headless Chromium holding a
 * WebGPU device never composites the canvas: `toDataURL`, a `drawImage` into a
 * 2D canvas and the compositor's own screenshot all come back blank while the
 * picture is perfectly correct. So the pixels come from `Viewport.readback`,
 * which copies the surface texture in the frame it drew — already RGBA, already
 * unpadded, already the canvas's backing-store size.
 *
 * What is left here is the two judgements made over them, and nothing that
 * knows where they came from.
 */

export interface Image {
  width: number;
  height: number;
  /** Bytes per pixel: 4 for the RGBA a readback is. */
  channels: number;
  /** Tightly packed pixels, row-major, `channels` bytes each. */
  data: Uint8Array;
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
 * of a megapixel frame.
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

/** FNV-1a over the pixels: two frames differ iff this differs. */
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
