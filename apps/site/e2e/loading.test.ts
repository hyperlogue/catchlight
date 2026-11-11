/** Download accounting must survive missing lengths and HTTP compression. */
import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { downloadModel } from "../src/loading.ts";

const originalFetch = globalThis.fetch;
const originalDocument = globalThis.document;
let label = "";
let detail = "";
let fraction: number | undefined;
let fractions: number[] = [];
beforeAll(() => { globalThis.document = {
  getElementById(id: string) {
    if (id === "loading-label") return { set textContent(value: string) { label = value; } };
    if (id === "loading-detail") return { set textContent(value: string) { detail = value; } };
    return {
      setAttribute() {},
      removeAttribute() { fraction = undefined; },
      set value(value: number) { fraction = value; fractions.push(value); },
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

for (const headers of [{}, { "content-length": "2", "content-encoding": "gzip" }]) {
  test(`bundled size gives progress before headers and during streaming: ${JSON.stringify(headers)}`, async () => {
    globalThis.fetch = (async () => {
      expect(detail).toBe("Model.clm · 0.0 MB / 2.0 MB · 0%");
      expect(fraction).toBe(0);
      let chunk = 0;
      return new Response(new ReadableStream({
        pull(controller) {
          if (chunk++ < 2) controller.enqueue(new Uint8Array(1048576));
          else controller.close();
        },
      }), { headers: headers as HeadersInit });
    }) as unknown as typeof fetch;
    fractions = [];
    const bytes = await downloadModel("https://example.test/model.clm", "Model.clm", 2097152);
    expect(fractions).toContain(0.5);
    expect(bytes.length).toBe(2097152);
    expect(detail).toBe("Model.clm · 2.0 MB / 2.0 MB · 100%");
    expect(fraction).toBe(1);
  });
}
