/**
 * The canvas, its renderer lifetime, and the camera's pan and zoom gestures.
 *
 * Attach and detach must be an exact inverse pair (StrictMode remounts); an
 * attach that resolves after cleanup is disposed on arrival. Pointer handlers
 * never read the canvas size, which would force layout on every move.
 */

import type { Camera, Session, Viewport as CoreViewport } from "@catchlight/core";
import { useCallback, useEffect, useRef, useState } from "react";
import type { ComponentProps, PointerEvent as ReactPointerEvent, Ref } from "react";

import { useEditor } from "./editor-context.js";
import { useLatest } from "./latest.js";
import {
  DEFAULT_CAMERA,
  fitCamera,
  panTo,
  worldAt,
  wheelNotches,
  ZOOM_PER_NOTCH,
  zoomAbout,
} from "./camera.js";
import type { Bounds, Point, Size } from "./camera.js";

/** A pointer over the canvas, in both frames a host might want it in. */
export interface ViewportPointerEvent {
  /** World units, Y-up. */
  world: Point;
  /** CSS pixels from the canvas's top-left corner. */
  screen: Point;
  event: ReactPointerEvent<HTMLCanvasElement>;
}

type PointerHandler = (event: ViewportPointerEvent) => void;
type OwnPointerProps = "onPointerDown" | "onPointerMove" | "onPointerUp" | "onPointerCancel";

// `onError` is also a DOM event on every element; this one wins.
export interface ViewportRootProps extends Omit<
  ComponentProps<"canvas">,
  OwnPointerProps | "onError"
> {
  /** What to draw. `undefined` keeps the canvas, drawing nothing, until one arrives. */
  session: Session | undefined;
  /** Controlled camera. With this set, the component moves only when the host says so. */
  camera?: Camera;
  /** Where an uncontrolled camera starts. Ignored once it has moved. */
  defaultCamera?: Camera;
  onCameraChange?: (camera: Camera) => void;
  /** The camera of the once-per-session auto-fit, reported after `onCameraChange`. */
  onFit?: (camera: Camera, bounds?: Bounds) => void;
  onResize?: (size: Size) => void;
  onPointerDown?: PointerHandler;
  onPointerMove?: PointerHandler;
  onPointerUp?: PointerHandler;
  onPointerCancel?: PointerHandler;
  /** Attach failure, e.g. neither WebGPU nor WebGL2. Unhandled, it is only a console warning. */
  onError?: (cause: unknown) => void;
  /** The hand tool pans with the primary pointer, without a Space modifier. */
  panMode?: boolean;
}

interface Pan {
  pointerId: number;
  camera: Camera;
  screen: Point;
}

export function ViewportRoot(props: ViewportRootProps) {
  const {
    session,
    camera,
    defaultCamera,
    onCameraChange,
    onFit,
    onResize,
    onPointerDown,
    onPointerMove,
    onPointerUp,
    onPointerCancel,
    onError,
    panMode,
    style,
    ref,
    ...rest
  } = props;
  const editor = useEditor();

  const [held, setHeld] = useState<Camera>(defaultCamera ?? DEFAULT_CAMERA);
  const current = camera ?? held;

  const view = useRef<CoreViewport | undefined>(undefined);
  const size = useRef<Size>({ width: 0, height: 0 });
  const origin = useRef<Point>([0, 0]);
  const pan = useRef<Pan | undefined>(undefined);
  const space = useRef(false);
  const fitted = useRef<number | undefined>(undefined);

  const cameraNow = useLatest(current);
  const controlled = useLatest(camera !== undefined);
  const changed = useLatest(onCameraChange);
  const framed = useLatest(onFit);
  const resized = useLatest(onResize);
  const forwarded = useLatest(ref);
  const autoFits = useLatest(defaultCamera === undefined);
  const failed = useLatest(onError);

  const commit = useCallback(
    (next: Camera): void => {
      cameraNow.current = next;
      if (!controlled.current) setHeld(next);
      changed.current?.(next);
    },
    [cameraNow, controlled, changed],
  );

  // Depends only on the session and editor: rebuilding it disposes a live renderer.
  const attach = useCallback(
    (canvas: HTMLCanvasElement) => {
      let live = true;
      let attached: CoreViewport | undefined;

      const remeasure = (): void => {
        const rect = canvas.getBoundingClientRect();
        origin.current = [rect.left, rect.top];
      };
      const measured = (width: number, height: number): void => {
        size.current = { width, height };
        remeasure();
        resized.current?.({ width, height });
      };
      measured(canvas.clientWidth, canvas.clientHeight);

      let observer: ResizeObserver | undefined;
      if (typeof ResizeObserver !== "undefined") {
        observer = new ResizeObserver((entries) => {
          const entry = entries[entries.length - 1];
          if (!entry) return;
          const box = entry.contentBoxSize?.[0];
          if (box) measured(box.inlineSize, box.blockSize);
          else measured(entry.contentRect.width, entry.contentRect.height);
        });
        observer.observe(canvas);
      }

      const onWheel = (event: WheelEvent): void => {
        event.preventDefault();
        const at = screenOf(event.clientX, event.clientY, origin.current);
        const factor = ZOOM_PER_NOTCH ** wheelNotches(event.deltaY, event.deltaMode);
        commit(zoomAbout(cameraNow.current, size.current, at, factor));
      };
      // Non-passive, so the page does not scroll while the canvas zooms.
      canvas.addEventListener("wheel", onWheel, { passive: false });
      globalThis.addEventListener?.("scroll", remeasure, {
        capture: true,
        passive: true,
      });
      globalThis.addEventListener?.("resize", remeasure, { passive: true });

      const releaseRef = applyRef(forwarded.current, canvas);

      let frame: number | undefined;
      if (session) {
        const drawn = session;
        // rAF callbacks run in queue order, so the renderer's tick that fills
        // the box has already run by the first attempt.
        let attempts = 0;
        const tryFit = (): void => {
          frame = undefined;
          if (!live || !autoFits.current || fitted.current === drawn.id) return;
          const bounds = drawn.bounds();
          const fit = fitCamera(bounds, size.current);
          if (fit) {
            fitted.current = drawn.id;
            commit(fit);
            framed.current?.(fit, bounds);
            return;
          }
          attempts += 1;
          if (attempts < AUTO_FIT_FRAMES) frame = requestFrame(tryFit);
        };

        void editor
          .attach(drawn, canvas)
          .then((viewport) => {
            if (!live) {
              viewport.dispose();
              return;
            }
            attached = viewport;
            view.current = viewport;
            const now = cameraNow.current;
            viewport.setCamera(now.center[0], now.center[1], now.height);
            viewport.start();
            frame = requestFrame(tryFit);
          })
          .catch((cause: unknown) => {
            const report = failed.current;
            if (report) report(cause);
            else console.warn("catchlight: attaching the viewport failed", cause);
          });
      }

      return () => {
        live = false;
        cancelFrame(frame);
        observer?.disconnect();
        canvas.removeEventListener("wheel", onWheel);
        globalThis.removeEventListener?.("scroll", remeasure, {
          capture: true,
        });
        globalThis.removeEventListener?.("resize", remeasure);
        releaseRef?.();
        pan.current = undefined;
        if (view.current === attached) view.current = undefined;
        attached?.dispose();
      };
    },
    [editor, session, commit, cameraNow, framed, resized, autoFits, forwarded, failed],
  );

  const [x, y] = current.center;
  useEffect(() => {
    view.current?.setCamera(x, y, current.height);
  }, [x, y, current.height]);

  // Space can go down before the pointer enters the canvas.
  useEffect(() => {
    const down = (event: KeyboardEvent): void => {
      const target = event.target instanceof HTMLElement ? event.target : null;
      if (
        event.code === "Space" &&
        !target?.matches("input,textarea,select,button,summary") &&
        !target?.isContentEditable
      ) {
        space.current = true;
        event.preventDefault();
      }
    };
    const up = (event: KeyboardEvent): void => {
      if (event.code === "Space") space.current = false;
    };
    const blur = (): void => {
      space.current = false;
    };
    globalThis.addEventListener?.("keydown", down);
    globalThis.addEventListener?.("keyup", up);
    globalThis.addEventListener?.("blur", blur);
    return () => {
      globalThis.removeEventListener?.("keydown", down);
      globalThis.removeEventListener?.("keyup", up);
      globalThis.removeEventListener?.("blur", blur);
    };
  }, []);

  const locate = (event: ReactPointerEvent<HTMLCanvasElement>): ViewportPointerEvent => {
    const screen = screenOf(event.clientX, event.clientY, origin.current);
    return {
      screen,
      world: worldAt(cameraNow.current, size.current, screen),
      event,
    };
  };

  const handleDown = (event: ReactPointerEvent<HTMLCanvasElement>): void => {
    const rect = event.currentTarget.getBoundingClientRect();
    origin.current = [rect.left, rect.top];
    capture(event);
    if (event.button === MIDDLE || (event.button === PRIMARY && (space.current || panMode))) {
      pan.current = {
        pointerId: event.pointerId,
        camera: cameraNow.current,
        screen: screenOf(event.clientX, event.clientY, origin.current),
      };
      return;
    }
    onPointerDown?.(locate(event));
  };

  const handleMove = (event: ReactPointerEvent<HTMLCanvasElement>): void => {
    const panning = pan.current;
    if (panning && panning.pointerId === event.pointerId) {
      const at = screenOf(event.clientX, event.clientY, origin.current);
      commit(panTo(panning.camera, panning.screen, size.current, at));
      return;
    }
    onPointerMove?.(locate(event));
  };

  const handleUp = (event: ReactPointerEvent<HTMLCanvasElement>): void => {
    release(event);
    if (endedPan(pan, event.pointerId)) return;
    onPointerUp?.(locate(event));
  };

  const handleCancel = (event: ReactPointerEvent<HTMLCanvasElement>): void => {
    release(event);
    if (endedPan(pan, event.pointerId)) return;
    onPointerCancel?.(locate(event));
  };

  return (
    <canvas
      data-catchlight-viewport=""
      // Behaviour: no inline baseline gap, and touch drags must not scroll the page.
      style={{ display: "block", touchAction: "none", ...style }}
      ref={attach}
      onPointerDown={handleDown}
      onPointerMove={handleMove}
      onPointerUp={handleUp}
      onPointerCancel={handleCancel}
      {...rest}
    />
  );
}

export const Viewport = { Root: ViewportRoot };

export interface ViewportCamera {
  camera: Camera;
  onCameraChange: (camera: Camera) => void;
  onFit: (camera: Camera, bounds?: Bounds) => void;
  onResize: (size: Size) => void;
  setCamera: (camera: Camera) => void;
  /** `false`, camera untouched, until the viewport has drawn a frame or when the model draws nothing. */
  fit: (session: Session) => boolean;
  /** Relative to the last fit (`1` at the fit); `undefined` before any fit. */
  zoom: number | undefined;
}

export function useViewportCamera(initial: Camera = DEFAULT_CAMERA): ViewportCamera {
  const [cameraState, setCamera] = useState<Camera>(initial);
  const [fitHeight, setFitHeight] = useState<number | undefined>(undefined);
  const size = useRef<Size>({ width: 0, height: 0 });
  const currentCamera = useLatest(cameraState);
  const lastFit = useRef<{ camera: Camera; bounds: Bounds } | undefined>(undefined);

  const onResize = useCallback((next: Size): void => {
    size.current = next;
    // Keep the fit's bounds so a moving pose never recentres a fitted view.
    const previous = lastFit.current;
    if (previous && previous.camera === currentCamera.current) {
      const framed = fitCamera(previous.bounds, next);
      if (framed) {
        lastFit.current = { camera: framed, bounds: previous.bounds };
        currentCamera.current = framed;
        setCamera(framed);
        setFitHeight(framed.height);
      }
    }
  }, []);

  const onFit = useCallback((framed: Camera, bounds?: Bounds): void => {
    lastFit.current = bounds ? { camera: framed, bounds } : undefined;
    setFitHeight(framed.height);
  }, []);

  const fit = useCallback((session: Session): boolean => {
    const bounds = session.bounds();
    const framed = fitCamera(bounds, size.current);
    if (!framed) return false;
    lastFit.current = bounds ? { camera: framed, bounds } : undefined;
    setCamera(framed);
    setFitHeight(framed.height);
    return true;
  }, []);

  const zoom = fitHeight === undefined ? undefined : fitHeight / cameraState.height;

  return {
    camera: cameraState,
    setCamera,
    onCameraChange: setCamera,
    onFit,
    onResize,
    fit,
    zoom,
  };
}

const PRIMARY = 0;
const MIDDLE = 1;

/** A model that draws nothing never gets a box; give up after ~4 s at 60 Hz. */
const AUTO_FIT_FRAMES = 240;

function requestFrame(callback: () => void): number | undefined {
  if (typeof requestAnimationFrame !== "function") return undefined;
  return requestAnimationFrame(callback);
}

function cancelFrame(frame: number | undefined): void {
  if (frame === undefined || typeof cancelAnimationFrame !== "function") return;
  cancelAnimationFrame(frame);
}

function screenOf(clientX: number, clientY: number, origin: Point): Point {
  return [clientX - origin[0], clientY - origin[1]];
}

function endedPan(pan: { current: Pan | undefined }, pointerId: number): boolean {
  if (pan.current?.pointerId !== pointerId) return false;
  pan.current = undefined;
  return true;
}

// Guarded because a non-browser DOM may not implement pointer capture.
function capture(event: ReactPointerEvent<HTMLCanvasElement>): void {
  const target = event.currentTarget;
  if (typeof target.setPointerCapture !== "function") return;
  try {
    target.setPointerCapture(event.pointerId);
  } catch {
    // The pointer is already gone.
  }
}

function release(event: ReactPointerEvent<HTMLCanvasElement>): void {
  const target = event.currentTarget;
  if (typeof target.releasePointerCapture !== "function") return;
  try {
    target.releasePointerCapture(event.pointerId);
  } catch {
    // Already released.
  }
}

function applyRef(ref: Ref<HTMLCanvasElement> | undefined, canvas: HTMLCanvasElement) {
  if (typeof ref === "function") {
    const cleanup = ref(canvas);
    return typeof cleanup === "function" ? cleanup : () => ref(null);
  }
  if (ref && typeof ref === "object") {
    ref.current = canvas;
    return () => {
      ref.current = null;
    };
  }
  return undefined;
}
