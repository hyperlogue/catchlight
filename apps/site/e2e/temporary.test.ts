import { expect, test } from "bun:test";
import { existsSync, rmSync } from "node:fs";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { removeTemporaryDirectory } from "./temporary.ts";

test("temporary cleanup finishes after a partial deletion reports EFAULT", async () => {
  const root = await mkdtemp(join(tmpdir(), "catchlight-cleanup-"));
  const cache = join(root, "cache");
  try {
    await mkdir(cache);
    await writeFile(join(cache, "chunk"), "cache data");
    await writeFile(join(root, "fixture"), "test data");
    let calls = 0;
    await removeTemporaryDirectory(root, async (path, options) => {
      if (++calls === 1) {
        await rm(cache, { recursive: true, force: true });
        throw Object.assign(new Error("bad address in system call argument"), { code: "EFAULT" });
      }
      await rm(path, options);
    });
    expect(existsSync(root)).toBe(false);
    // Repeating cleanup remains safe when the first attempt removed everything.
    await removeTemporaryDirectory(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("persistent cleanup faults still reject after a bounded number of attempts", async () => {
  const failure = Object.assign(new Error("persistent filesystem failure"), { code: "EFAULT" });
  let calls = 0;
  await expect(removeTemporaryDirectory("unused-fixture", async () => {
    calls++;
    throw failure;
  })).rejects.toBe(failure);
  expect(calls).toBe(4);
});

test("unrelated cleanup failures are reported immediately", async () => {
  const failure = Object.assign(new Error("permission denied"), { code: "EACCES" });
  let calls = 0;
  await expect(removeTemporaryDirectory("unused-fixture", async () => {
    calls++;
    throw failure;
  })).rejects.toBe(failure);
  expect(calls).toBe(1);
});
