import { expect, test } from "bun:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { createServer } from "vite";
import siteConfig from "../vite.config.js";

const executablePath = process.env.CHROMIUM ?? Bun.which("chromium");

test.skipIf(!executablePath)("the unconfigured site opens an empty editor without fetching a model", async () => {
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
    const models: string[] = [], errors: string[] = [];
    page.on("request", request => { if (new URL(request.url()).pathname.endsWith(".clm")) models.push(request.url()); });
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(server.resolvedUrls!.local[0]!, { waitUntil: "networkidle" });
    await page.getByRole("button", { name: "Create a model", exact: true }).waitFor();
    expect(await page.getByRole("button", { name: "Open a model", exact: true }).isVisible()).toBe(true);
    expect(await page.getByRole("button", { name: "Explore the starter character" }).count()).toBe(0);
    expect(await page.locator("[data-catchlight-session][data-current]").count()).toBe(0);
    expect(models).toEqual([]);
    expect(errors).toEqual([]);
  } finally {
    await browser.close();
    await server.close();
  }
}, 90000);
