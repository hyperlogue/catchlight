/**
 * The byte store: what a missing key does, and what runs where.
 */

import { describe, expect, test } from "bun:test";

import { MemoryStorage, NotFoundError, OpfsStorage } from "./storage.js";

describe("MemoryStorage", () => {
  test("reads back what it wrote, and lists it sorted", async () => {
    const storage = new MemoryStorage();
    await storage.write("b.clm", new TextEncoder().encode("b"));
    await storage.write("a.clm", new TextEncoder().encode("a"));
    await storage.write("nested/c.png", new TextEncoder().encode("c"));

    expect(await storage.list()).toEqual(["a.clm", "b.clm", "nested/c.png"]);
    expect(await storage.list("nested/")).toEqual(["nested/c.png"]);
    expect(await storage.read("a.clm")).toEqual(new TextEncoder().encode("a"));
  });

  test("a missing key is its own error, not a backend failure", async () => {
    const storage = new MemoryStorage();
    await expect(storage.read("gone.clm")).rejects.toBeInstanceOf(NotFoundError);
  });

  test("deleting is idempotent", async () => {
    const storage = new MemoryStorage();
    await storage.write("a.clm", new TextEncoder().encode("a"));
    await storage.delete("a.clm");
    await storage.delete("a.clm");
    expect(await storage.list()).toEqual([]);
  });
});

describe("OpfsStorage", () => {
  test("refuses up front where there is no origin private file system", async () => {
    // Under bun there is no OPFS, and a store that fails on the first read
    // instead of on construction is one every caller has to check.
    expect(OpfsStorage.available()).toBe(false);
    await expect(OpfsStorage.open()).rejects.toBeInstanceOf(Error);
  });
});
