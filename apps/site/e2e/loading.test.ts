/** Download accounting must survive missing lengths and HTTP compression. */
import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { downloadModel } from "../src/loading.ts";

const originalFetch = globalThis.fetch;
const originalDocument = globalThis.document;
let label = "";
let detail = "";
let fraction: number | undefined;
beforeAll(() => { globalThis.document = {
  getElementById(id: string) {
    if (id === "loading-label") return { set textContent(value: string) { label = value; } };
    if (id === "loading-detail") return { set textContent(value: string) { detail = value; } };
    return {
      setAttribute() {},
      removeAttribute() { fraction = undefined; },
      set value(value: number) { fraction = value; },
    };
  },
} as unknown as Document; });

afterEach(() => { globalThis.fetch = originalFetch; });
afterAll(() => { globalThis.document = originalDocument; });

for (const scenario of [
  { name: "known length", headers: { "content-length": "4" }, expected: 1 },
  { name: "unknown length", headers: {}, expected: undefined },
  { name: "compressed length", headers: { "content-length": "2", "content-encoding": "gzip" }, expected: undefined },
  { name: "incorrect length", headers: { "content-length": "2" }, expected: undefined },
]) {
  test(scenario.name, async () => {
    globalThis.fetch = (async () => new Response(new ReadableStream({
      start(controller) {
        controller.enqueue(new Uint8Array([1, 2]));
        controller.enqueue(new Uint8Array([3, 4]));
        controller.close();
      },
    }), { headers: scenario.headers as HeadersInit })) as unknown as typeof fetch;
    expect(await downloadModel("https://example.test/model.clm", "Model.clm")).toEqual(new Uint8Array([1, 2, 3, 4]));
    expect(label).toBe("Downloading model…");
    expect(detail).toContain("Model.clm");
    expect(fraction).toBe(scenario.expected);
  });
}

test("HTTP errors do not become model bytes", async () => {
  globalThis.fetch = (async () => new Response("missing", { status: 404 })) as unknown as typeof fetch;
  await expect(downloadModel("https://example.test/model.clm", "Model.clm")).rejects.toThrow("404");
});
