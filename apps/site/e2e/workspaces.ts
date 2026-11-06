/** Mesh and recording workflows on Lan with isolated authoring controls.
 * The probe reads assertions and separately simulates a concurrent editor. */
import assert from "node:assert/strict";
import { openModel, selectNode, prepareAuthoring } from "./model.ts";
import type { Editor } from "@catchlight/core";
import type { Page } from "playwright-core";

type Step = (name: string, work: () => Promise<void>) => Promise<void>;
export async function workspaces(
  page: Page,
  step: Step,
  shots: string,
  tag: string,
) {
  const button = (name: string) =>
    page.getByRole("button", { name, exact: true });
  const pause = () => page.waitForTimeout(140);
  const read = (node = "face") =>
    page.evaluate((node) => {
      const editor = (
        globalThis as unknown as { __catchlightProbe: { editor: Editor } }
      ).__catchlightProbe.editor;
      const id = Number(
        document
          .querySelector("[data-catchlight-session][data-current]")
          ?.getAttribute("data-session"),
      );
      const session = editor.session(id)!;
      return {
        revision: session.getRevision(),
        info: session.nodeInfo(node),
        mesh: session.nodeInfo(node)?.vertex_count ? session.mesh(node) : undefined,
        bindings: session.bindings(node),
        params: session.params(),
      };
    }, node);
  const changed = async (revision: number) => {
    await page.waitForFunction((revision) => {
      const editor = (
        globalThis as unknown as { __catchlightProbe: { editor: Editor } }
      ).__catchlightProbe.editor;
      const id = Number(
        document
          .querySelector("[data-catchlight-session][data-current]")
          ?.getAttribute("data-session"),
      );
      return editor.session(id)!.getRevision() > revision;
    }, revision);
    await pause();
  };
  const field = async (name: string, value: number) => {
    const revision = name.startsWith("Recorded ") ? (await read()).revision : undefined;
    const input = page.getByRole("spinbutton", { name, exact: true });
    await input.fill(String(value));
    await input.press("Enter");
    if (revision !== undefined) await changed(revision);
    else await pause();
  };
  const drag = async (
    selector: string,
    dx: number,
    dy: number,
    cancel = false,
  ) => {
    const index = await page.locator(selector).evaluateAll(elements => elements.findIndex(element => {
      const r = element.getBoundingClientRect();
      const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
      return hit !== null && element.contains(hit);
    }));
    assert(index >= 0, `no accessible handle for ${selector}`);
    const bounds = await page.locator(selector).nth(index).boundingBox();
    assert(bounds);
    const x = bounds.x + bounds.width / 2,
      y = bounds.y + bounds.height / 2;
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + dx, y + dy, { steps: 6 });
    if (cancel) await page.keyboard.press("Escape");
    await page.mouse.up();
    await pause();
  };
  let draftVertex = '[data-mesh-vertex="30"]';
  let shapeVertex = '[data-catchlight-vertex-handle][data-vertex="30"]';
  const pickHandles = async (selector: string) => page.locator(selector).evaluateAll(elements =>
    elements.filter(element => {
      const r = element.getBoundingClientRect();
      const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
      return hit === element;
    }).map(element => ({
      index: element.getAttribute("data-mesh-vertex") ?? element.getAttribute("data-vertex"),
      x: element.getBoundingClientRect().x, y: element.getBoundingClientRect().y,
    })));
  const meshFits = () => page.waitForFunction(() => {
    const canvas = document.querySelector("[data-catchlight-mesh-canvas]");
    const svg = canvas?.querySelector("svg");
    if (!canvas || !svg) return false;
    const bounds = canvas.getBoundingClientRect();
    const vertices = canvas.querySelectorAll("[data-catchlight-mesh-hit]");
    // ResizeObserver and React must publish the new size before checking the fit.
    return bounds.height > 280 && vertices.length > 0 &&
      svg.width.baseVal.value === canvas.clientWidth &&
      svg.height.baseVal.value === canvas.clientHeight &&
      [...vertices].every((vertex) => {
        const point = vertex.getBoundingClientRect();
        return point.left >= bounds.left && point.right <= bounds.right &&
          point.top >= bounds.top && point.bottom <= bounds.bottom;
      });
  });
  const armed = () =>
    page.locator("[data-catchlight-workspace][data-recording]").count();
  const canvas = page.locator("canvas[data-catchlight-viewport]");
  const open = async () => {
    await openModel(page);
    await prepareAuthoring(page);
    await selectNode(page, "face");
    const box = (await page.locator("[data-catchlight-selection-box]").boundingBox())!;
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.wheel(0, -1600);
    await pause();
  };

  await step("mesh draft keeps artwork fixed and applies once", async () => {
    await page.setViewportSize({ width: 1440, height: 960 });
    await open();
    const before = await read();
    assert(before.params.some(p => p.id === "test-drive"), "authoring controls are present before mesh editing");
    await button("Mesh").click();
    await page.locator("[data-catchlight-mesh-artwork]").waitFor();
    await meshFits();
    await page.setViewportSize({ width: 320, height: 844 });
    await meshFits();
    assert.equal((await read()).revision, before.revision);
    await page.screenshot({ path: `${shots}/${tag}-mesh-mobile.png` });
    await page.setViewportSize({ width: 1440, height: 960 });
    await meshFits();
    const handles = await pickHandles("[data-mesh-vertex]");
    assert(handles.length > 1, "Lan mesh exposes draggable vertex handles");
    draftVertex = `[data-mesh-vertex="${handles[0]!.index}"]`;
    const image = page.locator("[data-catchlight-mesh-artwork]");
    const mapping = await image.getAttribute("transform");
    await drag(draftVertex, 7, 5);
    assert.equal((await read()).revision, before.revision);
    assert.equal(await image.getAttribute("transform"), mapping);
    assert(await button("Apply mesh").isVisible());
    const x = await page
      .getByRole("spinbutton", { name: "Mesh vertex X", exact: true })
      .inputValue();
    await page.keyboard.press("Control+z");
    await pause();
    assert(await button("Done").isVisible());
    await page.keyboard.press("Control+Shift+z");
    await pause();
    assert.equal(
      await page
        .getByRole("spinbutton", { name: "Mesh vertex X", exact: true })
        .inputValue(),
      x,
    );
    await button("Arrange").click();
    await page.getByRole("dialog", { name: "Finish your mesh edit" }).waitFor();
    await button("Keep editing").click();
    await page.screenshot({ path: `${shots}/${tag}-mesh-draft.png` });
    await button("Apply mesh").click();
    await changed(before.revision);
    const applied = await read();
    assert.equal(applied.revision, before.revision + 1);
    assert.notDeepEqual(applied.mesh!.verts, before.mesh!.verts);
    assert.notDeepEqual(applied.mesh!.uvs, before.mesh!.uvs);
    // Every remaining UV still follows the same original texture mapping.
    const original = before.mesh!,
      next = applied.mesh!;
    for (const axis of [0, 1] as const) {
      const distinct = original.verts.reduce((best, p, i) =>
        Math.abs(p[axis] - original.verts[0]![axis]) >
        Math.abs(original.verts[best]![axis] - original.verts[0]![axis]) ? i : best, 0);
      const scale =
        (original.uvs[distinct]![axis] - original.uvs[0]![axis]) /
        (original.verts[distinct]![axis] - original.verts[0]![axis]);
      next.verts.forEach((p, i) =>
        assert(
          Math.abs(
            next.uvs[i]![axis] -
              (original.uvs[0]![axis] +
                (p[axis] - original.verts[0]![axis]) * scale),
          ) < 1e-5,
          `UV mapping changed at vertex ${i}, axis ${axis}`,
        ),
      );
    }
    await canvas.focus();
    await page.keyboard.press("Control+z");
    await changed(applied.revision);
    const undone = await read();
    assert.deepEqual(undone.mesh, before.mesh);
    assert.deepEqual(undone.params, before.params, "mesh undo preserves unrelated parameters");
  });

  await step(
    "mesh cancellation, multiple vertices, topology and local history",
    async () => {
      const before = await read();
      await button("Mesh").click();
      await page.locator("[data-catchlight-mesh-artwork]").waitFor();
      await pause();
      await drag(draftVertex, 7, 5, true);
      assert(await button("Done").isVisible());
      const anchor = await page.locator(draftVertex).boundingBox();
      assert(anchor);
      const ax = anchor.x + anchor.width / 2,
        ay = anchor.y + anchor.height / 2;
      await page.mouse.move(ax, ay);
      await page.mouse.down();
      await page.mouse.move(ax + 8, ay + 5, { steps: 4 });
      await page.mouse.move(ax, ay, { steps: 4 });
      await page.mouse.up();
      await pause();
      assert(
        await button("Done").isVisible(),
        "returning a drag to its start is a no-op",
      );
      const handles = await pickHandles("[data-mesh-vertex]");
      const first = handles[0]!;
      const second = handles.find(p => Math.hypot(p.x - first.x, p.y - first.y) > 40)!;
      assert(second, "two distinct mesh handles are accessible");
      draftVertex = `[data-mesh-vertex="${first.index}"]`;
      await page.locator(draftVertex).click();
      await page.keyboard.down("Shift");
      await page.locator(`[data-mesh-vertex="${second.index}"]`).click();
      await page.keyboard.up("Shift");
      assert.equal(
        await page
          .locator("[data-catchlight-draft-vertex][data-selected]")
          .count(),
        2,
      );
      await drag(draftVertex, 5, 4);
      assert.equal(
        await page
          .locator("[data-catchlight-draft-vertex][data-selected]")
          .count(),
        2,
      );
      await page.keyboard.press("Control+z");
      await pause();
      assert(await button("Done").isVisible());
      await button("Add vertex").click();
      const b = (await page.getByRole("application", { name: "Mesh on original artwork", exact: true }).boundingBox())!;
      await page.mouse.click(b.x + 50, b.y + b.height / 2);
      await pause();
      assert.equal(
        await page.locator("[data-mesh-vertex]").count(),
        before.info!.vertex_count! + 1,
      );
      await button("Delete selected vertices").click();
      await pause();
      assert.equal(
        await page.locator("[data-mesh-vertex]").count(),
        before.info!.vertex_count,
      );
      await button("Cancel").click();
      assert.equal((await read()).revision, before.revision);
    },
  );

  await step(
    "shape recording captures gestures and scrubbing never authors",
    async () => {
      const before = await read();
      await button("Record").click();
      await page.getByRole("combobox", { name: "Recording param", exact: true }).selectOption("test-drive");
      await button("Start recording").click();
      await pause();
      assert.equal((await read()).revision, before.revision);
      const handles = await pickHandles("[data-catchlight-vertex-handle]");
      assert(handles.length, "Lan shape exposes draggable vertex handles");
      shapeVertex = `[data-catchlight-vertex-handle][data-vertex="${handles[0]!.index}"]`;
      await drag(shapeVertex, 8, 5);
      await changed(before.revision);
      const keyed = await read();
      assert.equal(keyed.revision, before.revision + 1);
      assert.deepEqual(keyed.mesh, before.mesh);
      assert.equal(
        await page
          .locator("[data-catchlight-record-key][data-authored]")
          .count(),
        1,
      );
      assert.equal(await armed(), 1);
      await drag(shapeVertex, 5, 4, true);
      assert.equal((await read()).revision, keyed.revision);
      assert.equal(await armed(), 1);
      await page.screenshot({ path: `${shots}/${tag}-record-shape.png` });
      await field("Test drive recording value", 0.25);
      assert.equal(await armed(), 0);
      assert.equal((await read()).revision, keyed.revision);
      // Arming between existing positions authors nothing. The first gesture
      // adds a position only to its own deform binding, in the same revision.
      const beforeInsert = await read();
      await button("Start recording").click();
      await pause();
      assert.equal((await read()).revision, keyed.revision);
      assert.equal(await armed(), 1);
      await drag(shapeVertex, 3, 2);
      await changed(keyed.revision);
      const inserted = await read();
      const deform = inserted.bindings.find((b) => b.target === "deform" && b.param === "test-drive")!;
      const position = deform.key_positions[0]!.indexOf(0.625);
      assert(position >= 0);
      assert.equal(deform.authored[0]![position], true);
      assert.deepEqual(inserted.bindings.filter((b) => b.target !== "deform"), beforeInsert.bindings.filter((b) => b.target !== "deform"));
      await button("Stop recording").click();
      await field("Test drive recording value", 0.1);
      const rev = (await read()).revision;
      await button("Start recording").click();
      await pause();
      assert.equal(await armed(), 1);
      assert.equal((await read()).revision, rev);
      await canvas.focus();
      await page.keyboard.press("Control+z");
      await changed(rev);
      assert.equal(await armed(), 0);
    },
  );

  await step(
    "recorded transform fields and handles preserve authored base values",
    async () => {
      await button("Transform").click();
      await button("Start recording").click();
      const before = await read();
      await field("Recorded position X", 15);
      await changed(before.revision);
      await field("Recorded rotation", 15);
      await field("Recorded scale X", 1.1);
      const after = await read();
      assert.deepEqual(after.info!.translate, before.info!.translate);
      assert.deepEqual(after.info!.rotate, before.info!.rotate);
      assert.deepEqual(after.info!.scale, before.info!.scale);
      for (const target of ["tx", "rz", "sx"])
        assert(after.bindings.some((b) => b.target === target));
      await button("Rotate selection").focus();
      await page.keyboard.press("ArrowRight");
      await changed(after.revision);
      assert.deepEqual((await read()).info!.rotate, before.info!.rotate);
      const scaleRevision = (await read()).revision;
      await drag("[data-catchlight-selection-corner]", -12, -8);
      await changed(scaleRevision);
      assert.deepEqual((await read()).info!.scale, before.info!.scale);
      await selectNode(page, "mg-head");
      await pause();
      assert.equal(await armed(), 0);
      await button("Arrange").click();
    },
  );

  await step(
    "paired params retain both axes and record into the chosen cell",
    async () => {
      await button("Bindings").click();
      await page
        .getByRole("combobox", { name: "Param", exact: true })
        .selectOption("test-drive");
      await page
        .getByRole("combobox", { name: "Binding target", exact: true })
        .selectOption("tx");
      await page
        .getByRole("combobox", { name: "Second driving param", exact: true })
        .selectOption("test-pair");
      const before = await read("mg-head");
      await button("Bind").click();
      await changed(before.revision);
      await button("Record").click();
      await pause();
      await page
        .getByRole("combobox", { name: "Recording param", exact: true })
        .selectOption("test-pair");
      await pause();
      assert(await page.locator("[data-catchlight-record-matrix]").isVisible());
      await page
        .locator('[data-catchlight-record-key][data-key-x="2"][data-key-y="2"]')
        .click();
      await button("Start recording").click();
      const originalX = Number(await page.getByRole("spinbutton", { name: "Recorded position X", exact: true }).inputValue());
      await field("Recorded position X", 20);
      const result = await read("mg-head");
      const paired = result.bindings.find(
        (b) =>
          b.target === "tx" && b.param === "test-drive" && b.param_y === "test-pair",
      );
      assert(paired?.authored[2]?.[2]);
      assert(Math.abs(Number(paired.keys[2]![2]) - (20 - originalX)) < 1e-4);
      assert.equal(Number(await page.getByRole("spinbutton", { name: "Recorded position X", exact: true }).inputValue()), 20);
      assert(
        !result.bindings.some(
          (b) => b.target === "tx" && b.param === "test-pair" && !b.param_y,
        ),
      );
      assert.deepEqual(result.info!.translate, before.info!.translate);
      await page.screenshot({ path: `${shots}/${tag}-record-paired.png` });
      for (const width of [1024, 768, 390]) {
        await page.setViewportSize({ width, height: 900 });
        await pause();
        assert(
          await page.evaluate(
            () => document.documentElement.scrollWidth <= innerWidth,
          ),
        );
        await page.screenshot({
          path: `${shots}/${tag}-workspaces-${width}.png`,
        });
      }
      await page.setViewportSize({ width: 1440, height: 960 });
      await button("Stop recording").click();
      await button("Arrange").click();
    },
  );

  await step(
    "a concurrent edit preserves the draft and refuses stale Apply",
    async () => {
      await selectNode(page, "face");
      await button("Mesh").click();
      await page.locator("[data-catchlight-mesh-artwork]").waitFor();
      await pause();
      await drag(draftVertex, 6, 4);
      const revision = (await read()).revision;
      await page.evaluate(async () => {
        const editor = (
          globalThis as unknown as { __catchlightProbe: { editor: Editor } }
        ).__catchlightProbe.editor;
        const id = Number(
          document
            .querySelector("[data-catchlight-session][data-current]")
            ?.getAttribute("data-session"),
        );
        await editor.session(id)!.send({
          cmd: "node_set",
          node: "face",
          name: "Face updated elsewhere",
        });
      });
      await changed(revision);
      assert(await button("Apply mesh").isDisabled());
      assert(await button("Reload mesh").isVisible());
      assert(
        await page
          .locator("[data-catchlight-mesh-inspector]")
          .getByText("Unsaved mesh changes", { exact: true })
          .isVisible(),
      );
      await button("Reload mesh").click();
      await pause();
      assert(await button("Done").isVisible());
      await button("Done").click();
      assert.equal((await read()).info!.name, "Face updated elsewhere");
    },
  );
  await step(
    "a concurrent edit cancels a recording gesture without changing its key",
    async () => {
      await button("Record").click();
      await button("Transform").click();
      await button("Start recording").click();
      const before = await read();
      const corners = page.locator("[data-catchlight-selection-corner]");
      const index = await corners.evaluateAll(elements => elements.findIndex(element => {
        const r = element.getBoundingClientRect();
        const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
        return hit !== null && element.contains(hit);
      }));
      assert(index >= 0, "a transform handle is accessible for the concurrent gesture");
      const corner = await corners.nth(index).boundingBox();
      assert(corner);
      const x = corner.x + corner.width / 2,
        y = corner.y + corner.height / 2;
      await page.mouse.move(x, y);
      await page.mouse.down();
      await page.mouse.move(x + 10, y + 6, { steps: 4 });
      await page.evaluate(async () => {
        const editor = (
          globalThis as unknown as { __catchlightProbe: { editor: Editor } }
        ).__catchlightProbe.editor;
        const id = Number(
          document
            .querySelector("[data-catchlight-session][data-current]")
            ?.getAttribute("data-session"),
        );
        await editor
          .session(id)!
          .send({ cmd: "node_set", node: "Puppet", name: "Concurrent edit" });
      });
      await changed(before.revision);
      await page.mouse.up();
      await page.getByText("The model changed during this gesture. Start the gesture again.", { exact: true }).waitFor();
      const after = await read();
      assert.equal(after.revision, before.revision + 1);
      assert.deepEqual(after.bindings, before.bindings);
      assert.deepEqual(after.info, before.info);
      assert.equal(await armed(), 0);
      assert(
        await page
          .getByText(
            "The model changed during this gesture. Start the gesture again.",
            { exact: true },
          )
          .isVisible(),
      );
      await button("Dismiss notification").click();
      await button("Arrange").click();
    },
  );
}
