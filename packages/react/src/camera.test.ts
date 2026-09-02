/**
 * The arithmetic on its own, where an off-by-a-half-pixel is visible.
 */

import { describe, expect, test } from "bun:test";

import { panTo, screenAt, wheelNotches, worldAt, worldPerPixel, zoomAbout } from "./camera.js";

const size = { width: 800, height: 600 };
const camera = { center: [0, 0] as [number, number], height: 600 };

describe("screen against world", () => {
  test("the centre of the canvas is the centre of the camera", () => {
    expect(worldAt(camera, size, [400, 300])).toEqual([0, 0]);
  });

  test("one screen height is the camera's height, on both axes", () => {
    // The width follows from the aspect: nothing divides by the canvas width,
    // so a wide canvas shows more world rather than a stretched one.
    expect(worldPerPixel(camera, size)).toBe(1);
    expect(worldAt(camera, size, [800, 300])[0]).toBe(400);
    expect(worldAt({ ...camera, height: 1200 }, size, [800, 300])[0]).toBe(800);
  });

  test("world Y grows upward while screen Y grows downward", () => {
    expect(worldAt(camera, size, [400, 0])[1]).toBe(300);
    expect(worldAt(camera, size, [400, 600])[1]).toBe(-300);
  });

  test("placing a world point on the canvas is the inverse of reading one off it", () => {
    const screen: [number, number] = [123, 456];
    const round = screenAt(camera, size, worldAt(camera, size, screen));
    expect(round[0]).toBeCloseTo(screen[0], 9);
    expect(round[1]).toBeCloseTo(screen[1], 9);
  });

  test("a zero-height canvas does not divide by zero", () => {
    expect(Number.isFinite(worldPerPixel(camera, { width: 0, height: 0 }))).toBe(true);
  });
});

describe("zooming", () => {
  test("the world point under the cursor stays under the cursor", () => {
    const at: [number, number] = [640, 120];
    const before = worldAt(camera, size, at);
    const after = zoomAbout(camera, size, at, 1.1);
    expect(after.height).toBeCloseTo(660, 12);
    const moved = worldAt(after, size, at);
    expect(moved[0]).toBeCloseTo(before[0], 9);
    expect(moved[1]).toBeCloseTo(before[1], 9);
  });

  test("a camera height stays positive whatever it is multiplied by", () => {
    expect(zoomAbout(camera, size, [400, 300], 0).height).toBeGreaterThan(0);
    expect(Number.isFinite(zoomAbout(camera, size, [400, 300], Infinity).height)).toBe(true);
  });
});

describe("panning", () => {
  test("the world point the gesture grabbed stays under the pointer", () => {
    const from: [number, number] = [400, 300];
    const to: [number, number] = [500, 250];
    const grabbed = worldAt(camera, size, from);
    const after = panTo(camera, from, size, to);
    const under = worldAt(after, size, to);
    expect(under[0]).toBeCloseTo(grabbed[0], 9);
    expect(under[1]).toBeCloseTo(grabbed[1], 9);
  });

  test("a pan back to where it started restores the camera exactly", () => {
    // The property an incremental pan only approximates: every move is
    // measured from the gesture's own start, so nothing accumulates.
    const from: [number, number] = [400, 300];
    panTo(camera, from, size, [500, 250]);
    expect(panTo(camera, from, size, from).center).toEqual(camera.center);
  });
});

describe("wheel deltas", () => {
  test("each unit is read in what the browser measured it in", () => {
    expect(wheelNotches(100, 0)).toBe(1);
    expect(wheelNotches(3, 1)).toBe(1);
    expect(wheelNotches(1, 2)).toBe(1);
    // A trackpad's small pixel deltas stay small rather than rounding to a notch.
    expect(wheelNotches(4, 0)).toBeCloseTo(0.04, 12);
  });
});
