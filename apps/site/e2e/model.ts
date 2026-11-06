/** The private Lan model is supplied by the caller and never bundled with the site. */
import type { Page } from "playwright-core";

export function modelPath(): string {
  const path = process.env.OPEN_FILE ?? process.env.E2E_MODEL ?? process.env.CATCHLIGHT_LOCAL_MODEL;
  if (!path) throw new Error("Set E2E_MODEL to your private Lan.clm to run the browser suite.");
  return path;
}

export async function openModel(page: Page): Promise<void> {
  const previous = await page.evaluate(() => document.querySelector(
    "[data-catchlight-session][data-current]",
  )?.getAttribute("data-session") ?? null);
  if (previous !== null) {
    await page.locator(`[data-catchlight-session][data-session="${previous}"] [data-catchlight-session-close]`).click();
    const discard = page.getByRole("button", { name: "Close without saving", exact: true });
    await page.waitForFunction(id =>
      !document.querySelector(`[data-catchlight-session][data-session="${id}"]`) ||
      [...document.querySelectorAll('dialog[open], [role="dialog"]')].some(dialog => dialog.textContent?.includes("Close without saving")), previous);
    if (await discard.isVisible()) await discard.click();
    await page.locator(`[data-catchlight-session][data-session="${previous}"]`).waitFor({ state: "hidden" });
  }
  await page.locator("input[data-catchlight-file-open]").setInputFiles(modelPath());
  await page.waitForFunction((before) => {
    const current = document.querySelector("[data-catchlight-session][data-current]")?.getAttribute("data-session");
    return current != null && current !== before;
  }, previous, { timeout: 90000 });
}

export async function selectNode(page: Page, id: string): Promise<void> {
  const search = page.getByRole("textbox", { name: "Search nodes", exact: true });
  await search.fill(id.replaceAll("-", " "));
  const node = page.locator(`[data-catchlight-node][data-node="${id}"] [data-catchlight-node-label]`).first();
  // Some display names differ from their stable IDs.
  if (!await node.count()) await search.fill("");
  await node.click();
  await search.fill("");
}

/** Independent controls isolate authoring assertions from Lan's existing bindings. */
export async function prepareAuthoring(page: Page): Promise<void> {
  await page.evaluate(async () => {
    const { editor } = (globalThis as unknown as { __catchlightProbe: { editor: import("@catchlight/core").Editor } }).__catchlightProbe;
    const id = Number(document.querySelector("[data-catchlight-session][data-current]")!.getAttribute("data-session"));
    const session = editor.session(id)!;
    for (const [param, name] of [["test-drive", "Test drive"], ["test-pair", "Test pair"]] as const) {
      await session.send({ cmd: "param_add", param, name, min: -1, max: 1, default: 0 });
    }
    await session.send({ cmd: "edit_apply", if_rev: session.getRevision(), edits: [
      { op: "binding_add", param: "test-drive", node: "mg-head", target: "rz", key_positions: [[0, 0.5, 1]] },
      { op: "binding_cells_set", param: "test-drive", node: "mg-head", target: "rz", cells: [-0.18, 0, 0.18].map((value, i) => ({ cell: [i, 0], value: { scalar: value } })) },
    ] });
  });
}
