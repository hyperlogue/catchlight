/**
 * The only arithmetic in this package: canvas pixels against world units.
 *
 * Pure functions, separate from the component, because this is the piece that
 * silently puts a drag a few pixels off. World space is Y-up and screen space
 * is Y-down, and one screen *height* is `camera.height` world units on both
 * axes — the width follows from the aspect, which is why nothing here divides
 * by the canvas width.
 */

import type { Camera } from "@catchlight/core";

export type Point = [number, number];
export interface Size {
  width: number;
  height: number;
}

/** What the wasm viewport frames before anybody sets a camera. */
export const DEFAULT_CAMERA: Camera = { center: [0, 0], height: 2 };

/** World units per CSS pixel. */
export function worldPerPixel(camera: Camera, size: Size): number {
  return camera.height / Math.max(size.height, 1);
}

/** Where a point on the canvas lands in the world. */
export function worldAt(camera: Camera, size: Size, screen: Point): Point {
  const scale = worldPerPixel(camera, size);
  return [
    camera.center[0] + (screen[0] - size.width / 2) * scale,
    camera.center[1] - (screen[1] - size.height / 2) * scale,
  ];
}

/** Where a world point lands on the canvas — what an HTML overlay is placed by. */
export function screenAt(camera: Camera, size: Size, world: Point): Point {
  const scale = worldPerPixel(camera, size);
  return [
    size.width / 2 + (world[0] - camera.center[0]) / scale,
    size.height / 2 - (world[1] - camera.center[1]) / scale,
  ];
}

/**
 * Scales the camera by `factor` while the world point under `screen` stays
 * under `screen` — what a wheel over a canvas is expected to do.
 */
export function zoomAbout(camera: Camera, size: Size, screen: Point, factor: number): Camera {
  const height = clampHeight(camera.height * factor);
  const anchor = worldAt(camera, size, screen);
  const scale = height / Math.max(size.height, 1);
  return {
    center: [
      anchor[0] - (screen[0] - size.width / 2) * scale,
      anchor[1] + (screen[1] - size.height / 2) * scale,
    ],
    height,
  };
}

/**
 * The camera a pan reaches when a gesture that started at `from` with camera
 * `start` has arrived at `to`.
 *
 * Measured from where the gesture began rather than from the last move, so a
 * host that ignores an intermediate camera cannot make the pan drift.
 */
export function panTo(start: Camera, from: Point, size: Size, to: Point): Camera {
  const scale = worldPerPixel(start, size);
  return {
    center: [
      start.center[0] - (to[0] - from[0]) * scale,
      start.center[1] + (to[1] - from[1]) * scale,
    ],
    height: start.height,
  };
}

/**
 * A wheel event as notches. `deltaMode` is what the browser measured in:
 * pixels, lines or pages, and a trackpad reports many small pixel deltas
 * where a mouse reports one notch.
 */
export function wheelNotches(deltaY: number, deltaMode: number): number {
  if (deltaMode === 1) return deltaY / LINES_PER_NOTCH;
  if (deltaMode === 2) return deltaY;
  return deltaY / PIXELS_PER_NOTCH;
}

/** How much one notch zooms out. Scrolling down (`deltaY > 0`) shows more world. */
export const ZOOM_PER_NOTCH = 1.1;

/**
 * A camera height stays positive and finite; nothing else is clamped.
 *
 * There is no sensible absolute range: a model authored in pixels is framed
 * at eight thousand units and one authored in metres at two, and a component
 * that guessed would refuse to show one of them.
 */
function clampHeight(height: number): number {
  if (!Number.isFinite(height)) return MIN_HEIGHT;
  return Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, height));
}

const LINES_PER_NOTCH = 3;
const PIXELS_PER_NOTCH = 100;
const MIN_HEIGHT = 1e-6;
const MAX_HEIGHT = 1e9;
