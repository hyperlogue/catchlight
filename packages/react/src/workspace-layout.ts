/** Small layout behaviors shared by assembled workspaces. Preferences live
 * in browser storage; storage being unavailable never stops an editor. */
import { useCallback, useEffect, useRef, useState } from "react";
import type { KeyboardEvent, PointerEvent, RefObject } from "react";

export function usePanelSize(
  name: string,
  initial: number,
  min: number,
  max: number,
  axis: "x" | "y",
  direction = 1,
) {
  const [size, setSize] = useState(() => {
    try {
      const stored = localStorage.getItem(`catchlight.layout.${name}`);
      const n = Number(stored);
      if (stored && Number.isFinite(n)) return Math.max(min, Math.min(max, n));
    } catch {
      /* Storage is optional. */
    }
    return initial;
  });
  const active = useRef<{ pointer: number; start: number; size: number } | undefined>(undefined);
  const current = useRef(size);
  current.current = size;
  const save = (n: number) => {
    try {
      localStorage.setItem(`catchlight.layout.${name}`, String(n));
    } catch {
      /* Storage is optional. */
    }
  };
  const update = (n: number) => {
    const next = Math.max(min, Math.min(max, n));
    current.current = next;
    setSize(next);
    return next;
  };
  const coordinate = (e: PointerEvent) => (axis === "x" ? e.clientX : e.clientY);
  return {
    size,
    handle: {
      role: "separator" as const,
      tabIndex: 0,
      "aria-label": `Resize ${name}`,
      "aria-orientation": axis === "x" ? ("vertical" as const) : ("horizontal" as const),
      "aria-valuenow": size,
      "aria-valuemin": min,
      "aria-valuemax": max,
      onPointerDown: (e: PointerEvent<HTMLElement>) => {
        if (e.button !== 0) return;
        e.preventDefault();
        e.currentTarget.setPointerCapture(e.pointerId);
        active.current = { pointer: e.pointerId, start: coordinate(e), size };
      },
      onPointerMove: (e: PointerEvent<HTMLElement>) => {
        const drag = active.current;
        if (drag?.pointer === e.pointerId)
          update(drag.size + (coordinate(e) - drag.start) * direction);
      },
      onPointerUp: () => {
        if (active.current) save(current.current);
        active.current = undefined;
      },
      onPointerCancel: () => {
        if (active.current) update(active.current.size);
        active.current = undefined;
      },
      onLostPointerCapture: () => {
        active.current = undefined;
      },
      onDoubleClick: () => save(update(initial)),
      onKeyDown: (e: KeyboardEvent) => {
        const backward = axis === "x" ? "ArrowLeft" : "ArrowUp",
          forward = axis === "x" ? "ArrowRight" : "ArrowDown";
        if (e.key === backward || e.key === forward) {
          e.preventDefault();
          save(update(size + (e.key === backward ? -12 : 12) * direction));
        }
        if (e.key === "Home") {
          e.preventDefault();
          save(update(initial));
        }
      },
    },
  };
}

export function useDismissMenus(root: RefObject<HTMLElement | null>) {
  useEffect(() => {
    const close = (event: Event) => {
      const target = event.target instanceof Element ? event.target : null;
      root.current
        ?.querySelectorAll<HTMLDetailsElement>("details[data-catchlight-menu][open]")
        .forEach((menu) => {
          if (!target || !menu.contains(target)) menu.open = false;
        });
    };
    const escape = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape") return;
      const open = root.current?.querySelectorAll<HTMLDetailsElement>(
        "details[data-catchlight-menu][open]",
      );
      if (!open?.length) return;
      event.preventDefault();
      event.stopPropagation();
      open.forEach((menu) => {
        menu.open = false;
        menu.querySelector("summary")?.focus();
      });
    };
    document.addEventListener("pointerdown", close, true);
    document.addEventListener("keydown", escape, true);
    return () => {
      document.removeEventListener("pointerdown", close, true);
      document.removeEventListener("keydown", escape, true);
    };
  }, [root]);
}

/** Export the actual renderer's pixels; a WebGPU canvas's toDataURL can be
 * blank. The readback is the same explicit capture surface tests use. */
export function usePreviewExport(canvas: RefObject<HTMLCanvasElement | null>) {
  return useCallback(
    async (name: string) => {
      const capture =
        canvas.current &&
        (
          canvas.current as unknown as Record<
            string,
            () => Promise<{ width: number; height: number; rgba: Uint8Array }>
          >
        ).__catchlightReadback;
      if (!capture)
        throw new Error("The canvas is still starting. Try exporting again in a moment.");
      const frame = await capture();
      const image = document.createElement("canvas");
      image.width = frame.width;
      image.height = frame.height;
      image
        .getContext("2d")
        ?.putImageData(
          new ImageData(new Uint8ClampedArray(frame.rgba), frame.width, frame.height),
          0,
          0,
        );
      const blob = await new Promise<Blob | null>((resolve) => image.toBlob(resolve, "image/png"));
      if (!blob) throw new Error("The preview could not be exported.");
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `${name.replace(/\.clm$/i, "")}.png`;
      document.body.append(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    },
    [canvas],
  );
}
