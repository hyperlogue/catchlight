/** Editor workflows against real wasm, GPU and protocol. The probe only
 * reads state for assertions; every edit is made through the visible UI. */
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import type { Editor } from "@catchlight/core";
import type { Page } from "playwright-core";

type Step = (name: string, work: () => Promise<void>) => Promise<void>;
export async function studio(
  page: Page,
  step: Step,
  shots: string,
  tag: string,
) {
  const canvas = page.locator("canvas[data-catchlight-viewport]");
  const right = page.locator('[data-catchlight-panel="right"]');
  const read = (node = "root") =>
    page.evaluate(async (node) => {
      const { editor } = (
        globalThis as unknown as { __catchlightProbe: { editor: Editor } }
      ).__catchlightProbe;
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
        params: session.params(),
        bindings: session.bindings(node),
        tree: session.tree(),
        status: await session.queryServer({ cmd: "session_get" }),
        slots:
          session.nodeInfo(node)?.kind === "part"
            ? session.query({ cmd: "slot_list", node })
            : undefined,
      };
    }, node);
  const selected = (name: string) =>
    page.getByRole("button", { name, exact: true }).first().click();
  const change = async (before: number) => {
    await page.waitForFunction((before) => {
      const { editor } = (
        globalThis as unknown as { __catchlightProbe: { editor: Editor } }
      ).__catchlightProbe;
      const id = Number(
        document
          .querySelector("[data-catchlight-session][data-current]")
          ?.getAttribute("data-session"),
      );
      return (editor.session(id)?.getRevision() ?? 0) > before;
    }, before);
    await page.waitForTimeout(100);
  };
  const undo = async () => {
    const rev = (await read()).revision;
    await canvas.focus();
    await page.keyboard.press("Control+z");
    await change(rev);
  };
  const field = async (label: string, value: string) => {
    const input = page.getByRole("spinbutton", { name: label, exact: true });
    await input.fill(value);
    await input.press("Enter");
  };
  const disclosure = async (label: string) => {
    const summary = right
      .locator("summary")
      .filter({ hasText: new RegExp(`^${label}(?:$|\\d|\\s)`) })
      .first();
    const open = await summary.evaluate((el) =>
      el.parentElement?.hasAttribute("open"),
    );
    if (!open) await summary.click();
  };
  const drag = async (
    selector: string,
    dx: number,
    dy: number,
    cancel = false,
  ) => {
    const box = await page.locator(selector).first().boundingBox();
    assert(box, `no handle: ${selector}`);
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(
      box.x + box.width / 2 + dx,
      box.y + box.height / 2 + dy,
      { steps: 8 },
    );
    if (cancel) await page.keyboard.press("Escape");
    await page.mouse.up();
  };

  await step("starter, searchable tree, and artwork gallery", async () => {
    await page.setViewportSize({ width: 1440, height: 960 });
    await page
      .locator("input[data-catchlight-file-open]")
      .setInputFiles(
        fileURLToPath(new URL("../public/sample.clm", import.meta.url)),
      );
    await page.locator('[data-catchlight-node][data-node="face"]').waitFor();
    await page.waitForTimeout(700);
    const search = page.getByRole("textbox", { name: "Search nodes" });
    await search.fill("waving");
    assert.equal(await page.locator("[data-catchlight-node]").count(), 3);
    await search.fill("");
    await selected("Face");
    await page.getByRole("button", { name: "Artwork", exact: true }).click();
    assert.equal(await page.locator("[data-catchlight-asset]").count(), 13);
    assert(
      await page
        .locator("[data-catchlight-asset-thumb] canvas")
        .first()
        .evaluate((c) => (c as HTMLCanvasElement).width > 0),
    );
    await page
      .getByRole("button", { name: "Structure", exact: true })
      .first()
      .click();
  });

  await step("canvas handles commit once, cancel, and undo", async () => {
    const initial = await read("face");
    await drag("[data-catchlight-selection-corner]", -22, -12);
    await change(initial.revision);
    const scaled = await read("face");
    assert.notDeepEqual(scaled.info?.scale, initial.info?.scale);
    assert.equal(scaled.revision, initial.revision + 1);
    await undo();
    assert.deepEqual((await read("face")).info?.scale, initial.info?.scale);
    const beforeCancel = (await read()).revision;
    await drag("[data-catchlight-selection-corner]", -25, -15, true);
    await page.waitForTimeout(120);
    assert.equal((await read()).revision, beforeCancel);
    await page
      .getByRole("button", { name: "Rotate selection", exact: true })
      .focus();
    await page.keyboard.press("ArrowRight");
    await change(beforeCancel);
    assert((await read("face")).info!.rotate[2] < 0);
    await undo();
  });

  await step("properties validate drafts and display degrees", async () => {
    const before = await read("face");
    await field("Translate x", "12");
    await change(before.revision);
    assert.equal((await read("face")).info?.translate[0], 12);
    await field("Rotate z", "30");
    await page.waitForTimeout(120);
    assert(
      Math.abs((await read("face")).info!.rotate[2] - Math.PI / 6) < 0.0001,
    );
    const valid = (await read()).revision;
    await field("Opacity", "4");
    await page.waitForTimeout(120);
    assert.equal((await read()).revision, valid);
    assert.equal((await read("face")).info?.opacity, 1);
    await undo();
    await undo();
  });

  await step("pose controls and two-param binding authoring", async () => {
    await selected("Head");
    const before = (await read()).revision;
    await page
      .getByRole("spinbutton", { name: "Head tilt value", exact: true })
      .fill(".5");
    await page
      .getByRole("spinbutton", { name: "Head tilt value", exact: true })
      .press("Enter");
    assert.equal((await read()).revision, before);
    await page
      .getByRole("button", { name: "Preview sweep", exact: true })
      .click();
    await page.waitForTimeout(180);
    await page.getByRole("button", { name: "Stop sweep", exact: true }).click();
    assert.equal((await read()).revision, before);
    await page.getByRole("button", { name: "Reset pose", exact: true }).click();
    await page.getByRole("button", { name: "Bindings", exact: true }).click();
    await page
      .getByRole("combobox", { name: "Param", exact: true })
      .selectOption("head-tilt");
    await page
      .getByRole("spinbutton", { name: "rz cell 0,0", exact: true })
      .fill("-.25");
    await page
      .getByRole("spinbutton", { name: "rz cell 0,0", exact: true })
      .press("Enter");
    await change(before);
    assert.equal(
      (await read("head")).bindings.find((b) => b.target === "rz")
        ?.keys[0]?.[0],
      -0.25,
    );
    await page
      .getByRole("combobox", { name: "Binding target", exact: true })
      .selectOption("tx");
    await page
      .getByRole("combobox", { name: "Second driving param", exact: true })
      .selectOption("look");
    const rev = (await read()).revision;
    await page.getByRole("button", { name: "Bind", exact: true }).click();
    await change(rev);
    const two = (await read("head")).bindings.find(
      (b) => b.target === "tx" && b.param_y === "look",
    );
    assert.equal(two?.height, 2, "a new binding owns its two endpoint positions");
    const axesBefore = (await read("head")).bindings.find((b) => b.target === "rz")!.key_positions;
    const tx = page.locator('[data-catchlight-binding][data-target="tx"]');
    const firstAxis = tx.locator('[data-catchlight-param-keys][data-param="head-tilt"]');
    const insertRevision = (await read()).revision;
    await firstAxis.locator('[data-catchlight-param-key-insert]').click();
    await change(insertRevision);
    let edited = (await read("head")).bindings.find((b) => b.target === "tx" && b.param_y === "look")!;
    assert.deepEqual(edited.key_positions, [[0, 0.5, 1], [0, 1]]);
    const marker = firstAxis.locator('[data-catchlight-param-key][data-index="1"]');
    const markerBox = await marker.boundingBox(), trackBox = await firstAxis.locator('[data-catchlight-param-key-track]').boundingBox();
    assert(markerBox && trackBox);
    const moveRevision = (await read()).revision;
    await page.mouse.move(markerBox.x + markerBox.width / 2, markerBox.y + markerBox.height / 2);
    await page.mouse.down();
    await page.mouse.move(trackBox.x + trackBox.width * 0.65, markerBox.y + markerBox.height / 2, { steps: 5 });
    assert.equal((await read()).revision, moveRevision, "key position drag remains local until release");
    await page.mouse.up();
    await change(moveRevision);
    edited = (await read("head")).bindings.find((b) => b.target === "tx" && b.param_y === "look")!;
    assert(Math.abs(edited.key_positions[0]![1]! - 0.65) < 0.02);
    await page.screenshot({ path: `${shots}/${tag}-binding-axes.png` });
    const deleteRevision = (await read()).revision;
    await firstAxis.locator('[data-catchlight-param-key-delete]').click();
    await change(deleteRevision);
    edited = (await read("head")).bindings.find((b) => b.target === "tx" && b.param_y === "look")!;
    assert.deepEqual(edited.key_positions, [[0, 1], [0, 1]]);
    const secondAxis = tx.locator('[data-catchlight-param-keys][data-param="look"]');
    const secondRevision = (await read()).revision;
    await secondAxis.locator('[data-catchlight-param-key-insert]').click();
    await change(secondRevision);
    edited = (await read("head")).bindings.find((b) => b.target === "tx" && b.param_y === "look")!;
    assert.deepEqual(edited.key_positions, [[0, 1], [0, 0.5, 1]]);
    assert.deepEqual((await read("head")).bindings.find((b) => b.target === "rz")!.key_positions, axesBefore);
    await page.getByRole("button", { name: "Params", exact: true }).click();
    await page.getByRole("button", { name: "New param", exact: true }).click();
    await page
      .getByRole("dialog")
      .getByRole("textbox", { name: "Name", exact: true })
      .fill("Ear twitch");
    const rev2 = (await read()).revision;
    await page
      .getByRole("button", { name: "Create param", exact: true })
      .click();
    await change(rev2);
    assert((await read()).params.some((p) => p.name === "Ear twitch"));
  });

  await step("masks, slots, spine fitting, and model settings", async () => {
    await selected("Face");
    await disclosure("Masks");
    await page
      .getByRole("combobox", { name: "Mask source", exact: true })
      .selectOption("ear-left");
    let rev = (await read()).revision;
    await page.getByRole("button", { name: "Add mask", exact: true }).click();
    await change(rev);
    assert.equal((await read("face")).info?.masks?.length, 1);
    rev = (await read()).revision;
    await page
      .getByRole("button", { name: "Remove mask 1", exact: true })
      .click();
    await change(rev);
    await disclosure("Slots");
    rev = (await read()).revision;
    await page.getByRole("button", { name: "Add slot", exact: true }).click();
    await change(rev);
    const slots = (await read("face")).slots;
    assert(slots?.result === "slots" && slots.slots.length === 1);
    rev = (await read()).revision;
    await page.getByRole("button", { name: "Fill", exact: true }).click();
    await change(rev);
    const filled = (await read("face")).slots;
    assert(filled?.result === "slots" && filled.slots[0]?.vertex === 0);
    await selected("Scarf ribbon");
    await disclosure("Fit a spine");
    rev = (await read()).revision;
    await page
      .getByRole("button", { name: "Fit spine to artwork", exact: true })
      .click();
    await change(rev);
    await selected("Scarf ribbon spine");
    assert(
      (await read((await read("scarf-tail")).info!.parent!)).info?.spine?.chain,
    );
    await right.getByRole("button", { name: "Model", exact: true }).click();
    await disclosure("Physics environment");
    rev = (await read()).revision;
    await field("Global gravity", "7.5");
    await change(rev);
    const status = (await read()).status;
    assert(status.result === "status" && status.status.gravity === 7.5);
    await disclosure("Metadata");
    await page
      .getByRole("button", { name: "Add metadata", exact: true })
      .click();
    await page
      .getByRole("textbox", { name: "Key", exact: true })
      .fill("studio.bad key");
    await page
      .getByRole("textbox", { name: "JSON value", exact: true })
      .fill('{"note":"A little explorer"}');
    rev = (await read()).revision;
    await page
      .getByRole("button", { name: "Save metadata", exact: true })
      .click();
    await page.getByRole("dialog").getByRole("alert").waitFor();
    assert.equal((await read()).revision, rev);
    await page
      .getByRole("button", { name: "Dismiss notification", exact: true })
      .click();
    await page
      .getByRole("textbox", { name: "Key", exact: true })
      .fill("studio.notes");
    await page
      .getByRole("button", { name: "Save metadata", exact: true })
      .click();
    await change(rev);
    await right
      .getByRole("button", { name: "Properties", exact: true })
      .click();
  });

  await step(
    "artwork import, contour generation, and preview export",
    async () => {
      const png = await page.evaluate(() => {
        const art = document.createElement("canvas");
        art.width = 48;
        art.height = 40;
        const context = art.getContext("2d")!;
        context.fillStyle = "#edb177";
        context.fillRect(5, 5, 38, 30);
        return art.toDataURL("image/png").split(",")[1]!;
      });
      await page.getByLabel("Import artwork", { exact: true }).setInputFiles({
        name: "Studio badge.png",
        mimeType: "image/png",
        buffer: Buffer.from(png, "base64"),
      });
      await page
        .locator("[data-catchlight-node][data-selected]")
        .filter({ hasText: "Studio badge" })
        .waitFor();
      const id = (await page
        .locator("[data-catchlight-node][data-selected]")
        .getAttribute("data-node"))!;
      const imported = await read(id);
      assert.equal(imported.info?.vertex_count, 9);
      await disclosure("Mesh");
      await page.getByRole("button", { name: "Mesh", exact: true }).click();
      await disclosure("Generate mesh");
      await page
        .getByRole("button", { name: "Generate draft", exact: true })
        .click();
      await page
        .getByRole("button", { name: "Apply mesh", exact: true })
        .click();
      await change(imported.revision);
      const tracedModel = await read(id);
      const traced = tracedModel.info!;
      assert(traced.vertex_count! >= 4 && traced.triangle_count! >= 2);
      assert.notDeepEqual(tracedModel.mesh?.verts, imported.mesh?.verts);
      await page
        .getByRole("button", { name: "Delete selected node", exact: true })
        .click();
      await page
        .locator("[data-catchlight-node]")
        .filter({ hasText: /^Studio badge$/ })
        .waitFor({ state: "hidden" });
      await selected("Tail");
      await page
        .getByRole("button", { name: "Fit model (F)", exact: true })
        .click();
      await page
        .locator("[data-catchlight-menu] > summary")
        .filter({ hasText: /^File/ })
        .click();
      const downloaded = page.waitForEvent("download");
      await page
        .getByRole("button", { name: "Export preview…", exact: true })
        .click();
      const preview = await downloaded;
      assert(preview.suggestedFilename().endsWith(".png"));
      const path = await preview.path();
      assert(path && (await Bun.file(path).size) > 1000);
    },
  );

  await step("command search, layout sizing, and mobile panels", async () => {
    await canvas.focus();
    await page.keyboard.press("Control+k");
    await page
      .getByRole("combobox", { name: "Search commands and nodes", exact: true })
      .fill("tail");
    await page
      .getByRole("combobox", { name: "Search commands and nodes", exact: true })
      .press("Enter");
    assert.equal(
      await page
        .locator("[data-catchlight-node][data-selected]")
        .getAttribute("data-node"),
      "tail",
    );
    const before = (await page
      .locator('[data-catchlight-panel="left"]')
      .boundingBox())!.width;
    await page
      .getByRole("separator", { name: "Resize structure", exact: true })
      .focus();
    await page.keyboard.press("ArrowRight");
    assert(
      (await page.locator('[data-catchlight-panel="left"]').boundingBox())!
        .width > before,
    );
    await page.screenshot({ path: `${shots}/${tag}-studio-desktop.png` });
    await page.setViewportSize({ width: 390, height: 844 });
    assert(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    );
    await page
      .getByRole("button", { name: "Toggle structure panel", exact: true })
      .click();
    await page
      .locator('[data-catchlight-panel="left"]')
      .waitFor({ state: "visible" });
    assert(await page.locator('[data-catchlight-panel="left"]').evaluate(
      (panel) => panel.contains(document.activeElement),
    ), "opening a drawer moves keyboard focus inside it");
    await page.keyboard.press("Escape");
    await page.locator('[data-catchlight-panel="left"]').waitFor({ state: "hidden" });
    const structureToggle = page.getByRole("button", { name: "Toggle structure panel", exact: true });
    assert(await structureToggle.evaluate((button) => button === document.activeElement));
    await structureToggle.click();
    await page
      .getByRole("button", { name: "Close structure panel", exact: true })
      .click();
    await page
      .getByRole("button", { name: "Toggle properties panel", exact: true })
      .click();
    await right.waitFor({ state: "visible" });
    await page.screenshot({ path: `${shots}/${tag}-studio-mobile.png` });
    await page
      .getByRole("button", { name: "Close properties panel", exact: true })
      .click();
    const propertiesToggle = page.getByRole("button", { name: "Toggle properties panel", exact: true });
    assert(await propertiesToggle.evaluate((button) => button === document.activeElement));
    await propertiesToggle.click();
    await page.getByRole("button", { name: "Dismiss side panel", exact: true })
      .click({ position: { x: 8, y: 8 } });
    await right.waitFor({ state: "hidden" });
    assert(await propertiesToggle.evaluate((button) => button === document.activeElement));
    await page.setViewportSize({ width: 320, height: 844 });
    assert(await page.evaluate(() => {
      const navigation = document.querySelector("[data-catchlight-canvas-tools]")!.getBoundingClientRect();
      const zoom = document.querySelector("[data-catchlight-zoom-tools]")!.getBoundingClientRect();
      return document.documentElement.scrollWidth <= innerWidth && navigation.right <= zoom.left;
    }), "the narrow layout keeps canvas controls separate without page overflow");
    await page.setViewportSize({ width: 1440, height: 960 });
  });

  await step("save, dirty close protection, and a stable canvas", async () => {
    const inTab =
      (await page.locator("[data-catchlight-backend]").textContent()) ===
      "in-tab";
    const downloaded = inTab ? page.waitForEvent("download") : undefined;
    await page.locator("[data-catchlight-save]").click();
    const savedFile = downloaded ? await (await downloaded).path() : undefined;
    await page.waitForFunction(() =>
      document
        .querySelector("[data-catchlight-save-state]")
        ?.textContent?.includes("All changes saved"),
    );
    await selected("Face");
    const rev = (await read()).revision;
    await field("Translate x", "9");
    await change(rev);
    await page
      .locator(
        "[data-catchlight-session][data-current] [data-catchlight-session-close]",
      )
      .click();
    await page
      .getByRole("dialog", { name: "Save your changes?", exact: true })
      .waitFor();
    await page
      .getByRole("button", { name: "Keep editing", exact: true })
      .click();
    assert.equal((await read("face")).info?.translate[0], 9);
    const identity = await canvas.evaluate((c) => {
      c.setAttribute("data-kept-canvas", "yes");
      return true;
    });
    assert(identity);
    await page
      .locator(
        "[data-catchlight-session][data-current] [data-catchlight-session-close]",
      )
      .click();
    await page
      .getByRole("button", { name: "Close without saving", exact: true })
      .click();
    await page.waitForTimeout(250);
    assert.equal(await canvas.getAttribute("data-kept-canvas"), "yes");
    if (savedFile) {
      await page.locator("input[data-catchlight-file-open]").setInputFiles({
        name: "Round trip.clm",
        mimeType: "application/octet-stream",
        buffer: Buffer.from(await Bun.file(savedFile).arrayBuffer()),
      });
      await page.locator('[data-catchlight-node][data-node="face"]').waitFor();
      assert.equal((await read("face")).info?.translate[0], 0);
      assert((await read()).params.some((p) => p.name === "Ear twitch"));
    }
    const problem = await page.evaluate(
      () =>
        document.querySelector("[data-catchlight-problem]")?.textContent ?? "",
    );
    assert(!problem, problem ?? "");
  });
}
