/** Closing after Apply must protect the newly authored mesh before releasing the session. */
import assert from "node:assert/strict";
import { openModel, selectNode } from "./model.ts";
import type { Editor } from "@catchlight/core";
import type { Page } from "playwright-core";

type Step = (name: string, work: () => Promise<void>) => Promise<void>;

export async function draftClose(page: Page, step: Step, shots: string, tag: string) {
  await step("closing an applied mesh draft still protects unsaved model changes", async () => {
    await page.setViewportSize({ width: 1440, height: 960 });
    await openModel(page);
    await selectNode(page, "face");
    await page.locator("[data-catchlight-save]").click();
    await page.waitForFunction(() =>
      /^(Downloaded|Saved) /.test(
        document.querySelector("[data-catchlight-notice]")?.textContent ?? "",
      ),
    );

    const read = () => page.evaluate(async () => {
      const { editor } = (
        globalThis as unknown as { __catchlightProbe: { editor: Editor } }
      ).__catchlightProbe;
      const id = Number(document.querySelector("[data-catchlight-session][data-current]")
        ?.getAttribute("data-session"));
      const session = editor.session(id)!;
      return {
        id,
        revision: session.getRevision(),
        mesh: session.mesh("face"),
        info: (await editor.listSessions()).find((s) => s.session === id),
      };
    });
    const before = await read();
    assert.equal(before.info?.dirty, false, "the model starts saved");

    await page.getByRole("button", { name: "Mesh", exact: true }).click();
    const vertex = page.locator('[data-mesh-vertex="30"]');
    await vertex.waitFor();
    await vertex.focus();
    const x = page.getByRole("spinbutton", { name: "Mesh vertex X", exact: true });
    await x.fill(String(Number(await x.inputValue()) + 0.1));
    await x.press("Enter");
    await page.getByRole("button", { name: "Apply mesh", exact: true }).waitFor();
    const drafted = await read();
    assert.equal(drafted.revision, before.revision, "the draft has not authored the model");
    assert.equal(drafted.info?.dirty, false);

    await page.locator("[data-catchlight-session][data-current] [data-catchlight-session-close]").click();
    const finish = page.getByRole("dialog", { name: "Finish your mesh edit", exact: true });
    await finish.waitFor();
    await finish.getByRole("button", { name: "Apply & continue", exact: true }).click();

    const confirm = page.getByRole("dialog", { name: "Save your changes?", exact: true });
    await confirm.waitFor();
    assert(await confirm.getByRole("button", { name: "Save and close", exact: true }).isVisible());
    assert(await confirm.getByRole("button", { name: "Close without saving", exact: true }).isVisible());
    const applied = await read();
    assert.equal(applied.id, before.id);
    assert.equal(applied.revision, before.revision + 1);
    assert.equal(applied.info?.dirty, true);
    assert.notDeepEqual(applied.mesh.verts, before.mesh.verts);
    await page.screenshot({ path: `${shots}/${tag}-draft-close-confirmation.png` });

    await confirm.getByRole("button", { name: "Keep editing", exact: true }).click();
    await confirm.waitFor({ state: "hidden" });
    const kept = await read();
    assert.equal(kept.id, before.id);
    assert.equal(kept.info?.dirty, true);
    assert.deepEqual(kept.mesh, applied.mesh, "Keep editing preserves the applied mesh");
    assert.equal(await page.locator("[data-catchlight-workspace]").getAttribute("data-mode"), "arrange");
  });
}

export async function drawerKeyboard(page: Page, step: Step, shots: string, tag: string) {
  await step("drawers retain keyboard focus and let search recover before dismissal", async () => {
    await page.setViewportSize({ width: 1440, height: 960 });
    await openModel(page);
    await selectNode(page, "face");
    await page.setViewportSize({ width: 390, height: 844 });
    const toggle = page.getByRole("button", { name: "Toggle structure panel", exact: true });
    const panel = page.getByRole("dialog", { name: "Structure", exact: true });
    await toggle.click();
    await panel.waitFor();
    const search = panel.getByRole("textbox", { name: "Search nodes", exact: true });
    await search.fill("no matching node");
    await search.press("Escape");
    assert.equal(await search.inputValue(), "");
    assert(await panel.isVisible());
    assert(await search.evaluate((input) => input === document.activeElement));
    await search.press("Escape");
    await panel.waitFor({ state: "hidden" });
    assert(await toggle.evaluate((button) => button === document.activeElement));

    await toggle.click();
    const first = panel.getByRole("button", { name: "Structure", exact: true });
    assert(await first.evaluate((button) => button === document.activeElement));
    await page.keyboard.press("Shift+Tab");
    const last = panel.getByRole("button", { name: "Delete selected node", exact: true });
    assert(await last.evaluate((button) => button === document.activeElement));
    await page.keyboard.press("Tab");
    assert(await first.evaluate((button) => button === document.activeElement));

    await page.keyboard.press("Control+k");
    const commands = page.getByRole("dialog", { name: "Find your next move", exact: true });
    await commands.waitFor();
    await commands.getByRole("combobox", { name: "Search commands and nodes", exact: true }).focus();
    await page.keyboard.press("Tab");
    assert(await commands.evaluate((dialog) => dialog.contains(document.activeElement)));
    await page.keyboard.press("Escape");
    await commands.waitFor({ state: "hidden" });
    assert(await panel.isVisible());
    assert(await first.evaluate((button) => button === document.activeElement));
    await page.keyboard.press("Escape");
    await panel.waitFor({ state: "hidden" });

    await toggle.click();
    await page.keyboard.press("m");
    await page.locator('[data-catchlight-workspace][data-mode="mesh"]').waitFor();
    await panel.waitFor({ state: "hidden" });
    assert.equal(await page.locator("[data-mobile-panel]").count(), 0);
    assert(await toggle.isDisabled());
    assert(await page.getByRole("application", { name: "Mesh on original artwork", exact: true }).evaluate(
      (canvas) => canvas === document.activeElement,
    ), "entering Mesh dismisses the inert Structure drawer and restores usable focus");
    await page.getByRole("button", { name: "Arrange", exact: true }).click();

    const propertiesToggle = page.getByRole("button", { name: "Toggle properties panel", exact: true });
    await propertiesToggle.click();
    const properties = page.getByRole("dialog", { name: "Properties", exact: true });
    await properties.waitFor();
    const propertiesFirst = properties.getByRole("button", { name: "Properties", exact: true });
    await page.keyboard.press("Shift+Tab");
    assert(await properties.evaluate((dialog) => dialog.contains(document.activeElement)));
    assert(!(await propertiesFirst.evaluate((button) => button === document.activeElement)));
    await page.keyboard.press("Tab");
    assert(await propertiesFirst.evaluate((button) => button === document.activeElement));
    await page.screenshot({ path: `${shots}/${tag}-drawer-keyboard.png` });

    await page.setViewportSize({ width: 768, height: 960 });
    await page.waitForFunction(() => !document.querySelector("[data-mobile-panel]"));
    assert(await page.locator("canvas[data-catchlight-viewport]").evaluate(
      (canvas) => canvas === document.activeElement,
    ), "a breakpoint returns focus to the canvas when the drawer toggle is hidden");
    await page.setViewportSize({ width: 1440, height: 960 });
  });
}
