/**
 * The canvas, its lifetime, and the two gestures the camera owns.
 *
 * **The ref callback is the lifetime.** Attaching builds a renderer that holds
 * GPU memory, so attach and detach have to be a true inverse pair: mount,
 * unmount, mount again leaves exactly one live viewport, which is what
 * StrictMode checks on every development mount. `attach` is asynchronous, so
 * the cleanup may run before it resolves — the resolution is then disposed on
 * arrival rather than stored.
 *
 * **The canvas size is measured by the observer, never by a pointer handler.**
 * A pointer event that read `clientHeight` would force layout on every move,
 * which is exactly the frame the renderer wanted. The element's origin is read
 * when it resizes, when the page scrolls, and at the start of a gesture; its
 * size only when it resizes.
 *
 * **Wheel is a native listener, not `onWheel`.** React registers wheel
 * passively at the root, where `preventDefault` does nothing and the page
 * scrolls under the drawing.
 *
 * **Two gestures are the camera's, everything else is the host's.** Middle
 * drag pans, primary drag pans while Space is held, wheel zooms about the
 * cursor. Any other pointer event is handed up with its world position, and
 * this component takes no view on what it means.
 *
 * **A session is framed once, on the first frame it has anything to frame.**
 * The default camera is two world units at the origin, which for a model
 * authored in pixels is a hole punched through one cheek. The box that would
 * fix that is *posed*, so it does not exist until the renderer has ticked —
 * hence a retry on animation frames rather than a read at attach. Once per
 * session, never again: re-fitting on a revision would undo the user's own
 * zoom on every edit. A host that has its own idea where the camera starts
 * says so with `defaultCamera`, and that turns this off.
 *
 * **A canvas outlives its document.** `session` may be `undefined`: the
 * element is measured and listened to, and nothing is attached until a
 * session arrives — on the same element, which is the point. The camera, the
 * observers and the wheel listener are the element's and survive the document
 * that was drawn on it, so a host that swapped canvases between documents
 * would rebuild all three and hand the person a stage that jumped.
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
import type { Point, Size } from "./camera.js";

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

export interface ViewportRootProps extends Omit<ComponentProps<"canvas">, OwnPointerProps> {
  /** What to draw. `undefined` keeps the canvas, drawing nothing, until one arrives. */
  session: Session | undefined;
  /** Controlled camera. With this set, the component moves only when the host says so. */
  camera?: Camera;
  /** Where an uncontrolled camera starts. Ignored once it has moved. */
  defaultCamera?: Camera;
  onCameraChange?: (camera: Camera) => void;
  /**
   * The camera the component framed the session with by itself — the
   * once-per-session fit. Reported after `onCameraChange`, so a host that
   * owns the camera can tell the reference a zoom readout is relative to from
   * an ordinary move.
   */
  onFit?: (camera: Camera) => void;
  /**
   * The canvas's CSS size, whenever it changes. How a host that owns the
   * camera learns the aspect a fit has to frame against.
   */
  onResize?: (size: Size) => void;
  onPointerDown?: PointerHandler;
  onPointerMove?: PointerHandler;
  onPointerUp?: PointerHandler;
  onPointerCancel?: PointerHandler;
}

/** A pan in flight: the pointer, and where it started from. */
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
  /** The session this viewport has already framed, so it frames it once. */
  const fitted = useRef<number | undefined>(undefined);

  const cameraNow = useLatest(current);
  const controlled = useLatest(camera !== undefined);
  const changed = useLatest(onCameraChange);
  const framed = useLatest(onFit);
  const resized = useLatest(onResize);
  const forwarded = useLatest(ref);
  const autoFits = useLatest(defaultCamera === undefined);

  /** One place a new camera goes, wherever the gesture that made it started. */
  const commit = useCallback(
    (next: Camera): void => {
      cameraNow.current = next;
      if (!controlled.current) setHeld(next);
      changed.current?.(next);
    },
    [cameraNow, controlled, changed],
  );

  /**
   * Attaches a renderer to the canvas and follows the element while it lives.
   *
   * Depends on the session and the editor and nothing else: everything a
   * listener here needs from a later render it reads out of a box, because
   * rebuilding this callback would dispose a live renderer.
   */
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
      globalThis.addEventListener?.("scroll", remeasure, { capture: true, passive: true });
      globalThis.addEventListener?.("resize", remeasure, { passive: true });

      const releaseRef = applyRef(forwarded.current, canvas);

      let frame: number | undefined;
      if (session) {
        const drawn = session;
        // Framing the model, once it has been drawn once. A frame registered
        // here runs after the renderer's own — rAF callbacks run in the order
        // they were queued — so the tick that fills the box has already
        // happened by the first attempt.
        let attempts = 0;
        const tryFit = (): void => {
          frame = undefined;
          if (!live || !autoFits.current || fitted.current === drawn.id) return;
          const fit = fitCamera(drawn.bounds(), size.current);
          if (fit) {
            fitted.current = drawn.id;
            commit(fit);
            framed.current?.(fit);
            return;
          }
          attempts += 1;
          // A document that draws nothing never gets a box, and this must not
          // become a callback on every frame for the life of the tab.
          if (attempts < AUTO_FIT_FRAMES) frame = requestFrame(tryFit);
        };

        void editor
          .attach(drawn, canvas)
          .then((viewport) => {
            if (!live) {
              // The element was gone before the device answered.
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
            console.warn("catchlight: attaching the viewport failed", cause);
          });
      }

      return () => {
        live = false;
        cancelFrame(frame);
        observer?.disconnect();
        canvas.removeEventListener("wheel", onWheel);
        globalThis.removeEventListener?.("scroll", remeasure, { capture: true });
        globalThis.removeEventListener?.("resize", remeasure);
        releaseRef?.();
        pan.current = undefined;
        if (view.current === attached) view.current = undefined;
        attached?.dispose();
      };
    },
    [editor, session, commit, cameraNow, framed, resized, autoFits, forwarded],
  );

  // A camera the host changed. The one a gesture made has already been pushed
  // through here by the render it caused.
  const [x, y] = current.center;
  useEffect(() => {
    view.current?.setCamera(x, y, current.height);
  }, [x, y, current.height]);

  // Space is a modifier, so it is the keyboard's state and not the canvas's:
  // it can go down before the pointer ever enters the element.
  useEffect(() => {
    const down = (event: KeyboardEvent): void => {
      if (event.code === "Space") space.current = true;
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
    return { screen, world: worldAt(cameraNow.current, size.current, screen), event };
  };

  const handleDown = (event: ReactPointerEvent<HTMLCanvasElement>): void => {
    // The element's box can have moved since it was last measured, and this is
    // once per gesture rather than once per move.
    const rect = event.currentTarget.getBoundingClientRect();
    origin.current = [rect.left, rect.top];
    capture(event);
    if (event.button === MIDDLE || (event.button === PRIMARY && space.current)) {
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
      // The only styles this package writes, and both are behaviour: a canvas
      // is inline by default and would sit on a text baseline, and a touch
      // drag scrolls the page unless the element claims it.
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

/** What [`useViewportCamera`] hands back. Four of the seven are props. */
export interface ViewportCamera {
  /** Pass to `Viewport.Root` as `camera`. */
  camera: Camera;
  /** Pass to `Viewport.Root` as `onCameraChange`. */
  onCameraChange: (camera: Camera) => void;
  /** Pass to `Viewport.Root` as `onFit`. What [`zoom`] is measured against. */
  onFit: (camera: Camera) => void;
  /** Pass to `Viewport.Root` as `onResize`. What [`fit`] frames against. */
  onResize: (size: Size) => void;
  /** Moves the camera outright. */
  setCamera: (camera: Camera) => void;
  /**
   * Frames `session`'s model on the canvas this camera was last told the size
   * of — what a "Fit" button runs.
   *
   * `false` when there is nothing to frame: the box is posed, so a viewport
   * that has not drawn a frame yet has none, and neither does a document that
   * draws nothing. The camera is left alone in both cases.
   */
  fit: (session: Session) => boolean;
  /**
   * How far in the camera is, relative to the last fit: `1` at the framing a
   * fit produced, `2` at twice that, so a readout is `zoom * 100` percent.
   * `undefined` until something has fitted, because until then there is
   * nothing to be relative to.
   */
  zoom: number | undefined;
}

/**
 * Camera state for a host that wants to drive one.
 *
 * The canvas size is held in a ref rather than in state: nothing renders
 * differently for it, and a viewport that re-rendered its host on every
 * resize would do it in the middle of a resize. The height of the last fit is
 * state, because the readout it feeds does render differently.
 */
export function useViewportCamera(initial: Camera = DEFAULT_CAMERA): ViewportCamera {
  const [cameraState, setCamera] = useState<Camera>(initial);
  const [fitHeight, setFitHeight] = useState<number | undefined>(undefined);
  const size = useRef<Size>({ width: 0, height: 0 });

  const onResize = useCallback((next: Size): void => {
    size.current = next;
  }, []);

  const onFit = useCallback((framed: Camera): void => {
    setFitHeight(framed.height);
  }, []);

  const fit = useCallback((session: Session): boolean => {
    const framed = fitCamera(session.bounds(), size.current);
    if (!framed) return false;
    setCamera(framed);
    setFitHeight(framed.height);
    return true;
  }, []);

  const zoom = fitHeight === undefined ? undefined : fitHeight / cameraState.height;

  return { camera: cameraState, setCamera, onCameraChange: setCamera, onFit, onResize, fit, zoom };
}

const PRIMARY = 0;
const MIDDLE = 1;

/**
 * How many frames the auto-fit waits for geometry before giving up.
 *
 * A document that draws nothing never produces a box, and a callback on every
 * frame for the life of the tab is exactly what the renderer's own idling
 * rules exist to avoid. Four seconds at sixty hertz is long enough for a
 * model whose textures are still decoding.
 */
const AUTO_FIT_FRAMES = 240;

/**
 * One animation frame, where the host has them. A non-browser DOM does not,
 * and there the fit simply never runs — there is no picture to frame either.
 */
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

/** Ends a pan this pointer owns, reporting whether it did. */
function endedPan(pan: { current: Pan | undefined }, pointerId: number): boolean {
  if (pan.current?.pointerId !== pointerId) return false;
  pan.current = undefined;
  return true;
}

/**
 * Follows the pointer once it leaves the element, so a drag that ends off the
 * canvas still ends. Guarded because a non-browser DOM may not implement it.
 */
function capture(event: ReactPointerEvent<HTMLCanvasElement>): void {
  const target = event.currentTarget;
  if (typeof target.setPointerCapture !== "function") return;
  try {
    target.setPointerCapture(event.pointerId);
  } catch {
    // A pointer that is already gone cannot be captured, and need not be.
  }
}

function release(event: ReactPointerEvent<HTMLCanvasElement>): void {
  const target = event.currentTarget;
  if (typeof target.releasePointerCapture !== "function") return;
  try {
    target.releasePointerCapture(event.pointerId);
  } catch {
    // Already released, which is the state this wanted.
  }
}

/**
 * Hands the element to the ref a host passed, whichever of the two shapes it
 * is, and returns whatever has to be undone.
 */
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
