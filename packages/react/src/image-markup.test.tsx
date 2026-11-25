import "./test/setup.js";

import { describe, expect, test } from "bun:test";

import { ImageMarkup } from "./image-markup.js";
import { mount, run } from "./test/harness.js";

function pointer(type: string, x: number, y: number): Event {
  const event = new Event(type, { bubbles: true });
  Object.assign(event, { clientX: x, clientY: y, button: 0, pointerId: 1 });
  return event;
}

describe("marking up an image", () => {
  test("a drag draws the chosen shape in image pixels, and undo takes it back", async () => {
    const view = await mount(
      <ImageMarkup
        image={new Blob([new Uint8Array([1])], { type: "image/png" })}
        onCancel={() => {}}
        onDone={() => {}}
        onError={(cause) => {
          throw cause;
        }}
      />,
    );
    const picture = view.container.querySelector("img")!;
    Object.defineProperty(picture, "naturalWidth", { value: 400 });
    Object.defineProperty(picture, "naturalHeight", { value: 200 });
    await run(() => picture.dispatchEvent(new Event("load")));

    const surface = view.container.querySelector<SVGSVGElement>("[data-catchlight-markup-surface]")!;
    // Shown at half size: screen points map back to twice their value.
    surface.getBoundingClientRect = () =>
      ({ left: 0, top: 0, width: 200, height: 100 }) as DOMRect;
    await run(() => surface.dispatchEvent(pointer("pointerdown", 10, 10)));
    await run(() => surface.dispatchEvent(pointer("pointermove", 60, 40)));
    await run(() => surface.dispatchEvent(pointer("pointerup", 60, 40)));

    const box = surface.querySelector('[data-shape="box"]');
    expect(box?.getAttribute("x")).toBe("20");
    expect(box?.getAttribute("y")).toBe("20");
    expect(box?.getAttribute("width")).toBe("100");
    expect(box?.getAttribute("height")).toBe("60");

    await run(() =>
      view.container.querySelector<HTMLButtonElement>("[data-catchlight-markup-undo]")?.click(),
    );
    expect(surface.querySelector("[data-shape]")).toBeNull();
    await view.unmount();
  });
});
