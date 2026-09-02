/**
 * A box holding the newest value a render saw, for callbacks that must not be
 * rebuilt when it changes.
 *
 * The viewport's ref callback is the reason this exists: rebuilding it detaches
 * the canvas and disposes a GPU viewport, so it may depend on the session and
 * nothing else — while the handlers it installs still have to call whatever
 * `onCameraChange` the last render passed.
 */

import { useRef } from "react";
import type { RefObject } from "react";

export function useLatest<T>(value: T): RefObject<T> {
  const box = useRef(value);
  // Assigned in render on purpose: an effect would leave a native listener
  // that fires before the commit reading the previous render's value.
  box.current = value;
  return box;
}
