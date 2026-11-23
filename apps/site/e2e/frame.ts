/** Pixel statistics shared by renderer readbacks and decoded screenshots. */

export interface Image {
  width: number;
  height: number;
  /** Bytes per pixel. */
  channels: number;
  /** Tightly packed pixels, row-major, `channels` bytes each. */
  data: Uint8Array;
}

export interface Uniformity {
  /** Packed most-significant channel first. */
  colour: number;
  /** 0 to 1. */
  share: number;
}

/**
 * Boyer-Moore majority vote, then an exact count: only a share near 1 matters,
 * and this avoids a per-colour histogram over a megapixel frame. Without a
 * majority colour the share is merely below a half.
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

export function hex(colour: number, channels: number): string {
  return `#${colour.toString(16).padStart(channels * 2, "0")}`;
}

function pack(data: Uint8Array, at: number, channels: number): number {
  let key = 0;
  for (let c = 0; c < channels; c++) key = (key * 256 + data[at + c]!) >>> 0;
  return key;
}
