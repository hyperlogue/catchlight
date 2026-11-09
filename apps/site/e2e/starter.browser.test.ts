/** Startup and file routing; the E2E driver checks rendered presentation. */
import { expect, test } from "bun:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { createServer } from "vite";
import siteConfig from "../vite.config.js";

const executablePath = process.env.CHROMIUM ?? Bun.which("chromium");

test.skipIf(!executablePath)("the demo preloads Lan and reopens it from the empty state", async () => {
  const previous = process.env.CATCHLIGHT_LOCAL_MODEL;
  let config;
  try {
    delete process.env.CATCHLIGHT_LOCAL_MODEL;
    config = await siteConfig({ command: "serve", mode: "test", isPreview: false });
  } finally {
    if (previous !== undefined) process.env.CATCHLIGHT_LOCAL_MODEL = previous;
  }
  const server = await createServer({
    ...config,
    root: fileURLToPath(new URL("..", import.meta.url)),
    configFile: false,
    envDir: false,
    logLevel: "silent",
    server: { ...config.server, host: "127.0.0.1", port: 0 },
  });
  await server.listen();
  const browser = await chromium.launch({
    executablePath: executablePath!, headless: true,
    args: ["--no-sandbox", "--disable-dev-shm-usage", "--disable-gpu"],
  });
  try {
    const page = await browser.newPage();
    page.setDefaultTimeout(60000);
    const models: string[] = [], errors: string[] = [];
    page.on("request", request => { if (request.resourceType() === "fetch" && new URL(request.url()).pathname.endsWith(".clm")) models.push(request.url()); });
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(server.resolvedUrls!.local[0]!, { waitUntil: "networkidle" });
    const current = page.locator("[data-catchlight-session][data-current]");
    await current.waitFor();
    expect(await current.textContent()).toContain("Lan");
    expect(models).toHaveLength(1);
    await current.locator("[data-catchlight-session-close]").click();
    await page.getByRole("button", { name: "Create a model", exact: true }).waitFor();
    expect(await page.getByRole("button", { name: "Open a model", exact: true }).isVisible()).toBe(true);
    await page.getByRole("button", { name: "Explore the starter character" }).click();
    await current.waitFor();
    expect(await current.textContent()).toContain("Lan");
    expect(models).toHaveLength(2);
    expect(models[1]).toBe(models[0]);
    expect(errors).toEqual([]);
  } finally {
    await browser.close();
    await server.close();
  }
}, 90000);
