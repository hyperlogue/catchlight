/** Proposal A against the real editor. Visible controls author every user edit;
 * the probe reads assertions and separately simulates a concurrent editor. */
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
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
    const input = page.getByRole("spinbutton", { name, exact: true });
    await input.fill(String(value));
    await input.press("Enter");
    await pause();
  };
  const drag = async (
    selector: string,
    dx: number,
    dy: number,
    cancel = false,
  ) => {
    const bounds = await page.locator(selector).first().boundingBox();
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
  const draftVertex = '[data-mesh-vertex="30"]';
  const shapeVertex = '[data-catchlight-vertex-handle][data-vertex="30"]';
  const armed = () =>
    page.locator("[data-catchlight-workspace][data-recording]").count();
  const canvas = page.locator("canvas[data-catchlight-viewport]");
  const open = async () => {
    await page
      .locator("input[data-catchlight-file-open]")
      .setInputFiles(
        fileURLToPath(new URL("../public/sample.clm", import.meta.url)),
      );
    await page.locator('[data-catchlight-node][data-node="face"]').waitFor();
    await button("Face").click();
    await pause();
  };

  await step("mesh draft keeps artwork fixed and applies once", async () => {
    await page.setViewportSize({ width: 1440, height: 960 });
    await open();
    const before = await read();
    await button("Mesh").click();
    await page.locator("[data-catchlight-mesh-artwork]").waitFor();
    await pause();
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
    assert.notDeepEqual(applied.info!.mesh!.verts, before.info!.mesh!.verts);
    assert.notDeepEqual(applied.info!.mesh!.uvs, before.info!.mesh!.uvs);
    // Every remaining UV still follows the same original texture mapping.
    const original = before.info!.mesh!,
      next = applied.info!.mesh!;
    for (const axis of [0, 1] as const) {
      const distinct = original.verts.findIndex(
        (p) => Math.abs(p[axis] - original.verts[0]![axis]) > 1,
      );
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
        ),
      );
    }
    await canvas.focus();
    await page.keyboard.press("Control+z");
    await changed(applied.revision);
    assert.deepEqual((await read()).info!.mesh, before.info!.mesh);
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
      await page.locator(draftVertex).click();
      await page.keyboard.down("Shift");
      await page.locator('[data-mesh-vertex="31"]').click();
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
      const b = await page.locator(draftVertex).boundingBox();
      assert(b);
      await page.mouse.click(b.x + b.width / 2 + 18, b.y + b.height / 2 + 16);
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
      await button("Start recording").click();
      await pause();
      assert.equal((await read()).revision, before.revision);
      await drag(shapeVertex, 8, 5);
      await changed(before.revision);
      const keyed = await read();
      assert.equal(keyed.revision, before.revision + 1);
      assert.deepEqual(keyed.info!.mesh, before.info!.mesh);
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
      await field("Head tilt recording value", 0.25);
      assert.equal(await armed(), 0);
      assert.equal((await read()).revision, keyed.revision);
      // The gate must reveal itself even when the user left the shelf collapsed.
      await button("Collapse posing tools").click();
      await button("Start recording").click();
      await page
        .locator("[data-catchlight-key-gate]")
        .waitFor({ state: "visible" });
      assert.equal((await read()).revision, keyed.revision);
      await button("Add position & record").click();
      await changed(keyed.revision);
      assert.equal(
        (await read()).params.find((p) => p.id === "head-tilt")!.key_positions
          .length,
        4,
      );
      assert.equal(await armed(), 1);
      assert.equal(
        (await read()).bindings.find((b) => b.target === "deform")!
          .authored[0]![2],
        false,
      );
      await button("Stop recording").click();
      await field("Head tilt recording value", 0.1);
      const rev = (await read()).revision;
      await button("Start recording").click();
      await button("Snap & record").click();
      await pause();
      assert.equal(await armed(), 1);
      assert.equal((await read()).revision, rev);
      await canvas.focus();
      await page.keyboard.press("Control+z");
      await pause();
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
      await drag("[data-catchlight-selection-corner]", -12, -8);
      await pause();
      assert.deepEqual((await read()).info!.scale, before.info!.scale);
      await button("Head").click();
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
        .selectOption("head-tilt");
      await page
        .getByRole("combobox", { name: "Binding target", exact: true })
        .selectOption("tx");
      await page
        .getByRole("combobox", { name: "Second driving param", exact: true })
        .selectOption("look");
      const before = await read("head");
      await button("Bind").click();
      await changed(before.revision);
      await button("Record").click();
      await pause();
      await page
        .getByRole("combobox", { name: "Recording param", exact: true })
        .selectOption("look");
      await pause();
      assert(await page.locator("[data-catchlight-record-matrix]").isVisible());
      await page
        .locator('[data-catchlight-record-key][data-key-x="2"][data-key-y="2"]')
        .click();
      await button("Start recording").click();
      await field("Recorded position X", 20);
      const result = await read("head");
      const paired = result.bindings.find(
        (b) =>
          b.target === "tx" && b.param === "head-tilt" && b.param_y === "look",
      );
      assert(paired?.authored[2]?.[2]);
      assert.equal(paired.keys[2]![2], 20);
      assert(
        !result.bindings.some(
          (b) => b.target === "tx" && b.param === "look" && !b.param_y,
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
      await button("Face").click();
      await button("Mesh").click();
      await page.locator("[data-catchlight-mesh-artwork]").waitFor();
      await pause();
      await drag(draftVertex, 6, 4);
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
      await pause();
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
      const corner = await page
        .locator("[data-catchlight-selection-corner]")
        .first()
        .boundingBox();
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
          .send({ cmd: "node_set", node: "root", name: "Concurrent edit" });
      });
      await page.mouse.up();
      await pause();
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
