import { describe, expect, test } from "bun:test";

import { Editor } from "./editor.js";
import { FakeEditor, fakeWasm } from "./fakes.js";
import { InTabBackend } from "./in-tab.js";
import { MemoryStorage } from "./storage.js";

async function opened() {
  const module = fakeWasm();
  const wasm = new module.module.CatchlightEditor() as FakeEditor;
  const editor = await Editor.create(module.module, new InTabBackend(wasm, new MemoryStorage()));
  const session = await editor.newSession();
  const added = await session.send({ cmd: "param_add", name: "jaw", min: 0, max: 1, default: 0.2 });
  if (added.result !== "param") throw new Error("no param");
  return { wasm, session, jaw: added.param };
}

describe("the fakes' official extension commands", () => {
  test("a mapping is edited and read back through the replica", async () => {
    const { session, jaw } = await opened();
    const mapped = await session.send({ cmd: "humanoid_map", control: "mouth_open", param: jaw, nonstandard: false });
    expect(mapped).toEqual({
      result: "humanoid_mapped",
      mapping: { control: "mouth_open", param: jaw, stops: [0.2, 0.2, 1] },
      replaced: null,
    });
    const read = session.query({ cmd: "humanoid_get" });
    expect(read.result === "humanoid" && read.mappings).toEqual([
      { control: "mouth_open", param: jaw, stops: [0.2, 0.2, 1] },
    ]);
    await session.send({ cmd: "humanoid_unmap", control: "mouth_open" });
    const after = session.query({ cmd: "humanoid_get" });
    expect(after.result === "humanoid" && after.mappings).toEqual([]);
  });

  test("expressions and visemes are set, listed and deleted, and warnings come from the doc", async () => {
    const { wasm, session, jaw } = await opened();
    await session.send({
      cmd: "expression_set",
      group: "expression",
      name: "sad",
      settings: [{ param: jaw, value: 0.5 }],
      nonstandard: false,
    });
    const added = await session.send({ cmd: "viseme_add_standard", replace: false });
    expect(added.result === "visemes_added" && added.added.length).toBeGreaterThan(0);
    const listed = session.query({ cmd: "expression_list" });
    if (listed.result !== "expressions") throw new Error(listed.result);
    expect(listed.expressions.map((e) => e.name)).toEqual(["sad"]);
    expect(listed.visemes.length).toBe(added.result === "visemes_added" ? added.added.length : -1);

    const deleted = await session.send({ cmd: "expression_delete", group: "expression", name: "sad" });
    expect(deleted.result).toBe("expression_deleted");

    const warning = {
      key: "catchlight.humanoid",
      code: "catchlight.humanoid/shared-param",
      message: "shared",
      subjects: [{ param: jaw }],
    };
    wasm.docs.get(session.id)!.extensionWarnings.push(warning);
    await session.send({ cmd: "viseme_add_standard", replace: true });
    const warnings = session.query({ cmd: "extension_warnings" });
    expect(warnings.result === "extension_warnings" && warnings.warnings).toEqual([warning]);
  });
});
