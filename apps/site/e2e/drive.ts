// A headless-browser drive of the web editor, run by hand until cl-wtz wires it into CI.
// usage: bun run apps/site/e2e/drive.ts <chromium-exe> <site-url> [server-base]
//   OPEN_FILE=<path.clm>  opens a model through the file input
//   AGENT_CMD=<shell>     (connected only) an edit over the Unix socket that must reach the tab
//   SHOTS=<dir>           where canvas screenshots land
// Needs `playwright-core` on the import path (not a workspace dependency yet) and a Chromium
// that can run here; headless Chromium has no WebGPU, so this exercises the WebGL2 tier.
import { chromium } from "playwright-core";

const [exe, site, server] = process.argv.slice(2);
if (!exe || !site) throw new Error("usage: run.ts <chromium> <site-url> [server-base]");
const url = server ? `${site}?server=${encodeURIComponent(server)}` : site;
const shots = process.env.SHOTS ?? ".";
const tag = server ? "connected" : "intab";

const browser = await chromium.launch({
  executablePath: exe,
  headless: true,
  args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist", "--no-sandbox"],
});
const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
const logs: string[] = [];
page.on("console", (m) => logs.push(`[${m.type()}] ${m.text()}`));
page.on("pageerror", (e) => logs.push(`[pageerror] ${e.message}`));
page.on("response", (r) => { if (r.status() >= 400) logs.push(`[http ${r.status()}] ${r.url()}`); });

let failed = false;
const step = async (name: string, f: () => Promise<void>) => {
  try {
    await f();
    console.log(`ok   ${name}`);
  } catch (e) {
    failed = true;
    console.log(`FAIL ${name}: ${String(e).split("\n")[0]}`);
    await page.screenshot({ path: `${shots}/${tag}-fail-${name.replace(/\W+/g, "-")}.png` });
    throw e;
  }
};

let shotN = 0;
const canvasShot = async (): Promise<{ bytes: number; hash: number }> => {
  const buf = await page.locator("canvas[data-catchlight-viewport]").first().screenshot({
    path: `${shots}/${tag}-canvas-${shotN++}.png`,
  });
  let hash = 0;
  for (const b of buf) hash = (hash * 31 + b) >>> 0;
  return { bytes: buf.length, hash };
};

try {
  await step("load page", async () => {
    await page.goto(url, { waitUntil: "networkidle" });
    await page.waitForTimeout(1500);
    const pre = page.locator("pre#failure");
    const hidden = await pre.evaluate((el) => (el as HTMLElement).hidden).catch(() => true);
    if (!hidden) throw new Error(`page reported: ${(await pre.textContent())?.trim()}`);
  });
  await step("document open", async () => {
    await page.locator("canvas[data-catchlight-viewport]").first().waitFor({ timeout: 20000 });
    await page.locator("[data-catchlight-node]").first().waitFor({ timeout: 20000 });
    await page.waitForTimeout(1500);
  });
  if (server && process.env.AGENT_CMD) {
    await step("an agent's edit over the socket reaches the tab", async () => {
      const nodesBefore = await page.locator("[data-catchlight-node]").count();
      const statusBefore = (await page.locator("[data-catchlight-status]").textContent())?.trim();
      const proc = Bun.spawn(["bash", "-lc", process.env.AGENT_CMD!], { cwd: process.env.AGENT_CWD, stdout: "pipe", stderr: "pipe" });
      const out = await new Response(proc.stdout).text();
      const err = await new Response(proc.stderr).text();
      if ((await proc.exited) !== 0) throw new Error(`agent command failed: ${err.trim() || out.trim()}`);
      console.log("     agent said:", out.trim().split("\n")[0]);
      await page.waitForFunction((n) => document.querySelectorAll("[data-catchlight-node]").length > n, nodesBefore, { timeout: 10000 });
      await page.waitForTimeout(500);
      const statusAfter = (await page.locator("[data-catchlight-status]").textContent())?.trim();
      console.log("     status before/after:", statusBefore?.slice(0, 60), "|", statusAfter?.slice(0, 60));
      if (statusBefore === statusAfter) throw new Error("status line did not move");
    });
  }
  if (process.env.OPEN_FILE) {
    await step("open a .clm through the file input", async () => {
      const nodesBefore = await page.locator("[data-catchlight-node]").count();
      await page.locator("input[type=file][data-catchlight-file-open]").first().setInputFiles(process.env.OPEN_FILE!);
      const stem = process.env.OPEN_FILE!.split("/").pop()!.replace(/\.clm$/, "");
      await page.waitForFunction((s) => (document.querySelector("[data-catchlight-status]")?.textContent ?? "").includes(s), stem, { timeout: 15000 });
      void nodesBefore;
      await page.locator("[data-catchlight-param-slider]").first().waitFor({ timeout: 15000 });
      await page.waitForTimeout(1000);
      console.log("     status:", (await page.locator("[data-catchlight-status]").textContent())?.trim().slice(0, 80));
    });
  }
  let before = { bytes: 0, hash: 0 };
  await step("canvas draws something", async () => {
    before = await canvasShot();
    console.log("     canvas png bytes:", before.bytes);
    if (before.bytes < 1500) throw new Error(`canvas looks flat (${before.bytes} B png)`);
  });
  await step("slider poses the puppet", async () => {
    const slider = page.locator("[data-catchlight-param-slider]").first();
    if (!(await slider.count())) { console.log("     (no params in this model)"); return; }
    const max = await slider.getAttribute("max");
    await slider.evaluate((el, v) => {
      const input = el as HTMLInputElement;
      input.value = v; input.dispatchEvent(new Event("input", { bubbles: true }));
    }, max ?? "1");
    await page.waitForTimeout(800);
    const after = await canvasShot();
    if (after.hash === before.hash) throw new Error("canvas unchanged after slider");
    // Back to the rest pose, so the drag below moves a picture with structure in it.
    const min = await slider.getAttribute("min");
    await slider.evaluate((el, v) => {
      const input = el as HTMLInputElement;
      input.value = v; input.dispatchEvent(new Event("input", { bubbles: true }));
    }, min ?? "0");
    await page.waitForTimeout(500);
    before = await canvasShot();
  });
  await step("select a node and drag it", async () => {
    const items = page.locator("[data-catchlight-node]");
    const n = await items.count();
    const leaf = items.nth(n - 1);
    const clickable = leaf.locator("button, [role=button], span, label").first();
    if (await clickable.count()) await clickable.click(); else await leaf.click();
    await page.waitForTimeout(300);
    const status = await page.locator("[data-catchlight-status]").textContent();
    console.log("     status:", status?.trim().slice(0, 120));
    const box = (await page.locator("canvas[data-catchlight-viewport]").first().boundingBox())!;
    const cx = box.x + box.width / 2, cy = box.y + box.height / 2;
    const b0 = await canvasShot();
    await page.mouse.move(cx, cy);
    await page.mouse.down();
    for (let i = 1; i <= 10; i++) { await page.mouse.move(cx + i * 15, cy + i * 8); await page.waitForTimeout(40); }
    await page.waitForTimeout(200);
    const mid = await canvasShot();
    await page.mouse.up();
    await page.waitForTimeout(1500);
    const b1 = await canvasShot();
    const rev = await page.locator("[data-catchlight-status]").textContent();
    console.log("     hashes before/mid/after:", b0.hash, mid.hash, b1.hash, "| status:", rev?.trim().slice(0, 120));
    if (mid.hash === b0.hash) throw new Error("no live preview during drag");
    if (b1.hash === b0.hash) throw new Error("canvas unchanged after commit");
  });
  await page.screenshot({ path: `${shots}/${tag}-final.png` });
} catch {
  // reported by step
} finally {
  const bad = logs.filter((l) => /error|pageerror|warn|http 4|http 5/i.test(l));
  console.log(`console lines: ${logs.length}, errors/warnings: ${bad.length}`);
  for (const l of bad.slice(0, 15)) console.log("   ", l.slice(0, 300));
  await browser.close();
  process.exit(failed ? 1 : 0);
}
