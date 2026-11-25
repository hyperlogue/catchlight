/**
 * Marks up an image before it is attached to a review comment: boxes, arrows,
 * freehand strokes and a crop. Shapes are kept in image pixels and drawn as
 * SVG over the image; `Done` bakes them into a new PNG.
 */

import { useEffect, useRef, useState } from "react";
import type { PointerEvent } from "react";

import { Icon, Modal } from "./controls.js";
import type { IconName } from "./controls.js";

type Point = readonly [number, number];
type Tool = "box" | "arrow" | "pen" | "crop";

interface Shape {
  tool: Tool;
  /** Two corners for a box or crop, tail then head for an arrow, the path for a pen. */
  points: Point[];
}

const TOOLS: readonly (readonly [Tool, IconName, string])[] = [
  ["box", "grid", "Box"],
  ["arrow", "arrow", "Arrow"],
  ["pen", "spine", "Draw"],
  ["crop", "fit", "Crop"],
];

const INK = "#ff4d5e";

export interface ImageMarkupProps {
  image: Blob;
  onCancel: () => void;
  onDone: (marked: Blob) => void;
  onError: (cause: unknown) => void;
}

export function ImageMarkup({ image, onCancel, onDone, onError }: ImageMarkupProps) {
  const [url, setUrl] = useState<string>();
  const [size, setSize] = useState<{ width: number; height: number }>();
  const [tool, setTool] = useState<Tool>("box");
  const [shapes, setShapes] = useState<Shape[]>([]);
  const [draft, setDraft] = useState<Shape>();
  const picture = useRef<HTMLImageElement>(null);
  const surface = useRef<SVGSVGElement>(null);

  useEffect(() => {
    const made = URL.createObjectURL(image);
    setUrl(made);
    return () => URL.revokeObjectURL(made);
  }, [image]);

  const at = (event: PointerEvent<SVGSVGElement>): Point => {
    const box = event.currentTarget.getBoundingClientRect();
    const width = size?.width ?? 1;
    const height = size?.height ?? 1;
    const x = box.width > 0 ? ((event.clientX - box.left) / box.width) * width : 0;
    const y = box.height > 0 ? ((event.clientY - box.top) / box.height) * height : 0;
    return [Math.min(width, Math.max(0, x)), Math.min(height, Math.max(0, y))];
  };

  const begin = (event: PointerEvent<SVGSVGElement>): void => {
    if (!size || event.button !== 0) return;
    event.currentTarget.setPointerCapture?.(event.pointerId);
    const point = at(event);
    setDraft({ tool, points: [point, point] });
  };
  const move = (event: PointerEvent<SVGSVGElement>): void => {
    if (!draft) return;
    const point = at(event);
    setDraft(
      draft.tool === "pen"
        ? { tool: "pen", points: [...draft.points, point] }
        : { tool: draft.tool, points: [draft.points[0]!, point] },
    );
  };
  const end = (): void => {
    if (!draft) return;
    setDraft(undefined);
    if (extent(draft) < 3) return;
    // One crop at a time: a new one replaces the last.
    setShapes((was) => [...(draft.tool === "crop" ? was.filter((s) => s.tool !== "crop") : was), draft]);
  };

  const done = (): void => {
    const source = picture.current;
    if (!source || !size) return;
    bake(source, size, shapes).then(onDone, onError);
  };

  const drawn = draft ? [...shapes, draft] : shapes;
  const stroke = size ? Math.max(2, Math.round(Math.max(size.width, size.height) / 300)) : 2;
  return (
    <Modal title="Mark up image" wide onClose={onCancel}>
      <div data-catchlight-markup="">
        <div data-catchlight-markup-tools="" role="group" aria-label="Markup tools">
          {TOOLS.map(([each, icon, label]) => (
            <button
              key={each}
              type="button"
              aria-pressed={tool === each}
              onClick={() => setTool(each)}
            >
              <Icon name={icon} width="14" height="14" />
              {label}
            </button>
          ))}
          <button
            type="button"
            data-catchlight-markup-undo=""
            disabled={shapes.length === 0}
            onClick={() => setShapes((was) => was.slice(0, -1))}
          >
            <Icon name="undo" width="14" height="14" />
            Undo
          </button>
        </div>
        <div data-catchlight-markup-stage="">
          {url && (
            <img
              ref={picture}
              src={url}
              alt="The image being marked up"
              draggable={false}
              onLoad={(event) =>
                setSize({
                  width: event.currentTarget.naturalWidth,
                  height: event.currentTarget.naturalHeight,
                })
              }
            />
          )}
          {size && (
            <svg
              ref={surface}
              data-catchlight-markup-surface=""
              data-tool={tool}
              viewBox={`0 0 ${size.width} ${size.height}`}
              preserveAspectRatio="none"
              onPointerDown={begin}
              onPointerMove={move}
              onPointerUp={end}
              onPointerCancel={() => setDraft(undefined)}
            >
              {drawn.map((shape, index) => (
                <ShapeView key={index} shape={shape} stroke={stroke} size={size} />
              ))}
            </svg>
          )}
        </div>
        <div data-catchlight-dialog-actions="">
          <button type="button" onClick={onCancel}>
            Cancel
          </button>
          <button type="button" data-primary="" disabled={!size} onClick={done}>
            Done
          </button>
        </div>
      </div>
    </Modal>
  );
}

function extent(shape: Shape): number {
  const [first] = shape.points;
  if (!first) return 0;
  return Math.max(
    ...shape.points.map(([x, y]) => Math.max(Math.abs(x - first[0]), Math.abs(y - first[1]))),
  );
}

function rect(shape: Shape): { x: number; y: number; width: number; height: number } {
  const [a, b] = shape.points as [Point, Point];
  return {
    x: Math.min(a[0], b[0]),
    y: Math.min(a[1], b[1]),
    width: Math.abs(a[0] - b[0]),
    height: Math.abs(a[1] - b[1]),
  };
}

/** The two barbs of an arrow's head, as points. */
function barbs(tail: Point, head: Point, length: number): [Point, Point] {
  const angle = Math.atan2(head[1] - tail[1], head[0] - tail[0]);
  const barb = (turn: number): Point => [
    head[0] - length * Math.cos(angle + turn),
    head[1] - length * Math.sin(angle + turn),
  ];
  return [barb(0.45), barb(-0.45)];
}

function ShapeView({
  shape,
  stroke,
  size,
}: {
  shape: Shape;
  stroke: number;
  size: { width: number; height: number };
}) {
  const ink = { fill: "none", stroke: INK, strokeWidth: stroke, strokeLinecap: "round", strokeLinejoin: "round" } as const;
  if (shape.tool === "box") return <rect data-shape="box" {...rect(shape)} {...ink} />;
  if (shape.tool === "pen")
    return <polyline data-shape="pen" points={shape.points.map((p) => p.join(",")).join(" ")} {...ink} />;
  if (shape.tool === "arrow") {
    const [tail, head] = shape.points as [Point, Point];
    const [left, right] = barbs(tail, head, stroke * 5);
    return (
      <path
        data-shape="arrow"
        d={`M${tail.join(",")}L${head.join(",")}M${left.join(",")}L${head.join(",")}L${right.join(",")}`}
        {...ink}
      />
    );
  }
  const kept = rect(shape);
  return (
    <path
      data-shape="crop"
      fillRule="evenodd"
      fill="#000a"
      stroke="#fff"
      strokeWidth={Math.max(1, stroke / 2)}
      strokeDasharray={`${stroke * 3} ${stroke * 2}`}
      d={`M0,0H${size.width}V${size.height}H0ZM${kept.x},${kept.y}h${kept.width}v${kept.height}h${-kept.width}Z`}
    />
  );
}

async function bake(
  source: HTMLImageElement,
  size: { width: number; height: number },
  shapes: readonly Shape[],
): Promise<Blob> {
  const crop = shapes.find((shape) => shape.tool === "crop");
  const kept = crop ? rect(crop) : { x: 0, y: 0, ...size };
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(kept.width));
  canvas.height = Math.max(1, Math.round(kept.height));
  const context = canvas.getContext("2d");
  if (!context) throw new Error("This browser cannot draw the marked-up image.");
  context.translate(-kept.x, -kept.y);
  context.drawImage(source, 0, 0, size.width, size.height);
  context.strokeStyle = INK;
  context.lineWidth = Math.max(2, Math.round(Math.max(size.width, size.height) / 300));
  context.lineCap = "round";
  context.lineJoin = "round";
  for (const shape of shapes) {
    if (shape.tool === "crop") continue;
    context.beginPath();
    if (shape.tool === "box") {
      const box = rect(shape);
      context.rect(box.x, box.y, box.width, box.height);
    } else if (shape.tool === "pen") {
      shape.points.forEach(([x, y], index) =>
        index === 0 ? context.moveTo(x, y) : context.lineTo(x, y),
      );
    } else {
      const [tail, head] = shape.points as [Point, Point];
      const [left, right] = barbs(tail, head, context.lineWidth * 5);
      context.moveTo(...tail);
      context.lineTo(...head);
      context.moveTo(...left);
      context.lineTo(...head);
      context.lineTo(...right);
    }
    context.stroke();
  }
  const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/png"));
  if (!blob) throw new Error("The marked-up image could not be saved.");
  return blob;
}
