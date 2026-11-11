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
    await page.getByRole("heading", { name: "Bring a character to life." }).waitFor();
    expect(models).toHaveLength(0);
    await page.getByRole("link", { name: /Open the editor/ }).click();
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
    await page.getByRole("link", { name: "← All demos" }).click();
    await page.getByRole("link", { name: /Try face tracking/ }).click();
    await page.getByRole("button", { name: "Start camera" }).waitFor();
    expect(await page.locator("video").evaluate(video => (video as HTMLVideoElement).srcObject)).toBeNull();
    await page.getByRole("button", { name: "Start camera" }).click();
    await page.getByRole("status").filter({ hasText: "Tracking stopped" }).waitFor();
    expect(await page.getByRole("button", { name: "Start camera" }).isVisible()).toBe(true);
    // Supply deterministic detections while using a real browser MediaStream.
    // This checks the Lan mapping and shutdown without a physical webcam.
    await page.route(/@mediapipe_tasks-vision.*\.js/, route => route.fulfill({
      contentType: "application/javascript",
      body: `export const FilesetResolver = { forVisionTasks: async () => ({}) };
        export const FaceLandmarker = { createFromOptions: async () => ({
          close() { globalThis.detectorClosed = true; },
          detectForVideo() { return { faceLandmarks: [[]], facialTransformationMatrixes: [],
            faceBlendshapes: [{ categories: [
              { categoryName: "eyeBlinkLeft", score: 1 },
              { categoryName: "jawOpen", score: 0.5 }
            ] }] }; }
        }) };`,
    }));
    await page.goto(`${server.resolvedUrls!.local[0]}?demo=face&probe=1`);
    await page.getByRole("button", { name: "Start camera" }).waitFor();
    await page.evaluate(() => {
      const canvas = document.createElement("canvas");
      canvas.width = canvas.height = 32;
      const context = canvas.getContext("2d")!;
      // A canvas stream only emits frames when the canvas changes.
      let shade = 0;
      setInterval(() => { context.fillStyle = `rgb(${shade = (shade + 1) % 256}, 0, 0)`; context.fillRect(0, 0, 32, 32); }, 33);
      const stream = canvas.captureStream(30);
      Object.defineProperty(navigator.mediaDevices, "getUserMedia", { value: async () => stream });
      (globalThis as unknown as { testStream: MediaStream }).testStream = stream;
    });
    await page.getByRole("button", { name: "Start camera" }).click();
    await page.getByRole("status").filter({ hasText: "Tracking live" }).waitFor();
    const readPose = () => page.evaluate(async () => {
      const { editor } = (globalThis as unknown as { __catchlightProbe: { editor: import("@catchlight/core").Editor } }).__catchlightProbe;
      const session = editor.session((await editor.listSessions())[0]!.session)!;
      return session.params().filter(p => ["eye_left_open", "eye_right_open", "mouth_open"].includes(p.name))
        .map(p => ({ name: p.name, value: session.paramValue(p.id), default: p.default }));
    });
    // Mirrored by default: the tracked left eye closes the eye on that side of the screen.
    let pose = await readPose();
    expect(pose.find(p => p.name === "eye_right_open")?.value).toBe(0);
    expect(pose.find(p => p.name === "mouth_open")?.value).toBe(0.75);
    await page.getByRole("switch", { name: "Mirror left and right" }).uncheck();
    await page.waitForFunction(() => !document.querySelector("video")!.classList.contains("mirrored"));
    for (let tries = 0; tries < 100 && (await readPose()).find(p => p.name === "eye_left_open")?.value !== 0; tries++)
      await page.waitForTimeout(100);
    pose = await readPose();
    expect(pose.find(p => p.name === "eye_left_open")?.value).toBe(0);
    await page.getByRole("switch", { name: "Stats for nerds" }).check();
    const stats = page.getByRole("region", { name: "Stats for nerds" });
    await stats.getByText("mouth_open").waitFor();
    expect(await page.locator(".tier-badge").textContent()).toMatch(/^WebGPU$|^WebGL2$/);
    await page.getByRole("button", { name: "Stop camera" }).click();
    expect(await page.evaluate(() => {
      const state = globalThis as unknown as { testStream: MediaStream; detectorClosed: boolean };
      return state.detectorClosed && state.testStream.getTracks().every(t => t.readyState === "ended");
    })).toBe(true);
    for (const param of await readPose()) expect(param.value).toBe(param.default);
    await page.getByRole("link", { name: "← All demos" }).click();
    await page.getByRole("heading", { name: "Bring a character to life." }).waitFor();
  } finally {
    await browser.close();
    await server.close();
  }
}, 90000);
