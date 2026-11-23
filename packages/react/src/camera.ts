/**
 * Canvas pixels against world units. World space is Y-up, screen space Y-down;
 * one screen height is `camera.height` world units on both axes.
 */

import type { Camera } from "@catchlight/core";

export type Point = [number, number];
/** A world-space box, as the replica reports it: `[min_x, min_y, max_x, max_y]`. */
export type Bounds = [number, number, number, number];
export interface Size {
  width: number;
  height: number;
}

export const DEFAULT_CAMERA: Camera = { center: [0, 0], height: 2 };

/** World units per CSS pixel. */
export function worldPerPixel(camera: Camera, size: Size): number {
  return camera.height / Math.max(size.height, 1);
}

export function worldAt(camera: Camera, size: Size, screen: Point): Point {
  const scale = worldPerPixel(camera, size);
  return [
    camera.center[0] + (screen[0] - size.width / 2) * scale,
    camera.center[1] - (screen[1] - size.height / 2) * scale,
  ];
}

/** World point to canvas CSS pixels, for placing HTML overlays. */
export function screenAt(camera: Camera, size: Size, world: Point): Point {
  const scale = worldPerPixel(camera, size);
  return [
    size.width / 2 + (world[0] - camera.center[0]) / scale,
    size.height / 2 - (world[1] - camera.center[1]) / scale,
  ];
}

/** Scales by `factor`, keeping the world point under `screen` fixed. */
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

/** Measured from the gesture's start, not the last move, so skipped cameras cannot drift. */
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
 * `undefined` for a missing or non-finite box, so the caller keeps its
 * camera. `padding` is a fraction of the box's extent; a box with no extent is
 * centred at the default height.
 */
export function fitCamera(
  bounds: Bounds | undefined,
  size: Size,
  padding: number = FIT_PADDING,
): Camera | undefined {
  if (!bounds) return undefined;
  const [minX, minY, maxX, maxY] = bounds;
  if (!bounds.every((value) => Number.isFinite(value))) return undefined;
  const width = maxX - minX;
  const height = maxY - minY;
  if (width < 0 || height < 0) return undefined;

  const aspect = size.width > 0 && size.height > 0 ? size.width / size.height : 1;
  const covering = Math.max(height, width / aspect);
  const framed = covering > 0 ? covering * (1 + Math.max(0, padding)) : DEFAULT_CAMERA.height;
  return { center: [(minX + maxX) / 2, (minY + maxY) / 2], height: clampHeight(framed) };
}

/** `deltaMode` is pixels, lines or pages; a trackpad sends many small pixel deltas. */
export function wheelNotches(deltaY: number, deltaMode: number): number {
  if (deltaMode === 1) return deltaY / LINES_PER_NOTCH;
  if (deltaMode === 2) return deltaY;
  return deltaY / PIXELS_PER_NOTCH;
}

/** Zoom-out factor per notch; `deltaY > 0` shows more world. */
export const ZOOM_PER_NOTCH = 1.1;

// Only positivity and finiteness are clamped: models span from metres to
// thousands of pixels, so no absolute range fits all.
function clampHeight(height: number): number {
  if (!Number.isFinite(height)) return MIN_HEIGHT;
  return Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, height));
}

const FIT_PADDING = 0.35;
const LINES_PER_NOTCH = 3;
const PIXELS_PER_NOTCH = 100;
const MIN_HEIGHT = 1e-6;
const MAX_HEIGHT = 1e9;
