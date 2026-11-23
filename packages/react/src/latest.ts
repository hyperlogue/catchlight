/** The newest render value, for callbacks that must not be rebuilt when it changes. */

import { useRef } from "react";
import type { RefObject } from "react";

export function useLatest<T>(value: T): RefObject<T> {
  const box = useRef(value);
  // Assigned in render on purpose: an effect would leave a native listener
  // that fires before the commit reading the previous render's value.
  box.current = value;
  return box;
}
