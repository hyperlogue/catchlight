/** Check export receipts using the browser's independent SHA-256 implementation,
 * and verify a fork is an immediately usable model with independent history. */
import assert from "node:assert/strict";
import type { Editor } from "@catchlight/core";
import type { Page } from "playwright-core";

export async function snapshots(page: Page): Promise<void> {
  const result = await page.evaluate(async () => {
    const { editor } = (globalThis as unknown as { __catchlightProbe: { editor: Editor } }).__catchlightProbe;
    const id = Number(document.querySelector("[data-catchlight-session][data-current]")?.getAttribute("data-session"));
    const source = editor.session(id)!;
    const before = source.getRevision();
    const exported = await editor.exportSession(source);
    const digest = Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", new Uint8Array(exported.bytes))))
      .map((byte) => byte.toString(16).padStart(2, "0")).join("");
    const fork = await editor.forkSession(source, "Snapshot candidate");
    try {
      const forkRevision = fork.getRevision();
      const sourceChildren = source.tree().children.length;
      await fork.send({ cmd: "node_add", parent: fork.tree().id, kind: "group", name: "Independent edit" });
      const independent = source.getRevision() === before && source.tree().children.length === sourceChildren
        && fork.tree().children.length === sourceChildren + 1;
      return { bytes: exported.bytes.byteLength, length: exported.byte_length, digestMatches: digest === exported.sha256,
        format: exported.format, forkRevision, independent };
    } finally {
      await editor.closeSession(fork.id);
    }
  });
  assert(result.bytes > 0);
  assert.equal(result.bytes, result.length);
  assert.equal(result.format, "clm");
  assert(result.digestMatches, "SHA-256 receipt must identify the exact exported bytes");
  assert.equal(result.forkRevision, 0);
  assert(result.independent, "editing a fork must preserve the source model and revision");
}
