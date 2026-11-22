/**
 * The downloadable editor end to end: `catchlight-editor` serves this page,
 * a browser opens it, and an agent drives the same session through
 * `catchlight-editor <command>` while the tab watches.
 *
 * Needs the launcher built with its page compiled in:
 *
 * ```text
 * cargo xtask bundle-ui --debug && cargo build -p catchlight-editor
 * bun run --filter catchlight-local-editor e2e
 *   CATCHLIGHT_EDITOR_BIN=<exe>  overrides target/debug/catchlight-editor
 *   CHROMIUM=<exe>               overrides the `chromium` on PATH
 * ```
 *
 * Everything the launcher and the cli share — the connection file, the
 * remembered session, the default socket — is pointed into a temporary
 * directory, so the run neither finds nor disturbs an editor the developer
 * already has open.
 */
import { expect, test } from "bun:test";
import { existsSync } from "node:fs";
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";

const root = fileURLToPath(new URL("../../..", import.meta.url));
const exe = process.env.CATCHLIGHT_EDITOR_BIN ?? join(root, "target/debug/catchlight-editor");
const browserExe = process.env.CHROMIUM ?? Bun.which("chromium");
const model = join(root, "tests/models/blend_modes.clm");

test.skipIf(!browserExe || !existsSync(exe))(
  "an agent's session and edits reach the tab the launcher serves",
  async () => {
    const dir = await mkdtemp(join(tmpdir(), "catchlight-launcher-"));
    const connection = join(dir, "connection.json");
    const env = {
      ...process.env,
      CATCHLIGHT_EDITOR_CONNECTION: connection,
      XDG_RUNTIME_DIR: dir,
      XDG_CACHE_HOME: join(dir, "cache"),
    };
    const cli = (...args: string[]) => {
      const run = Bun.spawnSync([exe, ...args], { env, stderr: "pipe" });
      const out = run.stdout.toString();
      if (run.exitCode !== 0) throw new Error(`${args.join(" ")}: ${run.stderr.toString()}${out}`);
      return out;
    };

    const server = Bun.spawn([exe, "--no-open", "--port", "0"], { env, stderr: "pipe" });
    const browser = await chromium.launch({
      executablePath: browserExe!,
      headless: true,
      args: ["--no-sandbox", "--disable-dev-shm-usage", "--disable-gpu"],
    });
    try {
      const deadline = Date.now() + 30_000;
      while (!existsSync(connection)) {
        if (Date.now() > deadline) throw new Error("the launcher never wrote its connection file");
        await Bun.sleep(50);
      }
      const { url } = JSON.parse(await readFile(connection, "utf8")) as { url: string };
      if ((await fetch(url)).status !== 200) {
        throw new Error(`${exe} serves no page; run \`cargo xtask bundle-ui\` and rebuild it`);
      }

      const page = await browser.newPage();
      page.setDefaultTimeout(60_000);
      const errors: string[] = [];
      page.on("pageerror", (error) => errors.push(error.message));
      await page.goto(url);
      await page.getByRole("button", { name: "Open a model", exact: true }).waitFor();

      // The agent opens a model; the tab lists it without a reload.
      expect(cli("session", "open", model)).toContain("session 1");
      const listed = page.locator("[data-catchlight-session]").filter({ hasText: "blend_modes" });
      await listed.waitFor();
      if ((await page.locator("[data-catchlight-session][data-current]").count()) === 0) {
        await listed.click();
      }
      await page.locator("[data-catchlight-session][data-current]").filter({ hasText: "blend_modes" }).waitFor();

      // An agent edit lands in the tab's replica.
      cli("node", "set", "node-2", "--name", "Renamed by agent");
      await page.getByText("Renamed by agent").first().waitFor();

      // A second launch finds the first instead of starting another.
      const again = Bun.spawnSync([exe, "--no-open"], { env, stderr: "pipe" });
      expect(again.exitCode).toBe(0);
      expect(again.stderr.toString()).toContain("already running");
      expect(errors).toEqual([]);
    } finally {
      await browser.close();
      server.kill("SIGINT");
      await server.exited;
    }
    // Ctrl-C takes the connection file with it.
    expect(existsSync(connection)).toBe(false);
  },
  120_000,
);
