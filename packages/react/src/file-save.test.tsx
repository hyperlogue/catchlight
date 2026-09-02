/**
 * Saving: the command, then the bytes out of the tab.
 */

import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import { Editor } from "@catchlight/core";
import { fakeWasm, readStructure, ScriptedBackend } from "@catchlight/core/fakes";

import { EditorProvider, FileSave, saveKey, useFileSave } from "./index.js";
import type { FileSaver, SaveOutcome } from "./index.js";
import { fire, harness, mount, settle } from "./test/harness.js";

describe("saving as", () => {
  test("sends the key typed and downloads the bytes the store holds", async () => {
    const { editor, wasm } = await harness();
    const session = await editor.newDocument("akari");
    const download = stubDownload();
    const outcomes: SaveOutcome[] = [];

    const view = await mount(
      <EditorProvider editor={editor}>
        <FileSave.Root session={session} onSaved={(outcome) => outcomes.push(outcome)} />
      </EditorProvider>,
    );
    const input = view.container.querySelector<HTMLInputElement>("[data-catchlight-save-as]");
    const form = view.container.querySelector("form");
    if (!input || !form) throw new Error("no save-as form");

    input.value = "copy";
    await fire(form, new Event("submit", { bubbles: true, cancelable: true }));
    await settle();
    await settle();

    // The name typed became a key with the extension a later open reads.
    expect(wasm.requests.find((request) => request.cmd === "save")).toMatchObject({
      path: "copy.clm",
    });
    expect(outcomes).toEqual([{ key: "copy.clm", downloaded: true }]);

    // And the download is the store's copy of the document, under that name.
    expect(download.names).toEqual(["copy.clm"]);
    const blob = download.blobs[0];
    if (!blob) throw new Error("nothing was downloaded");
    expect(readStructure(new Uint8Array(await blob.arrayBuffer())).title).toBe("akari");

    await view.unmount();
    download.restore();
  });

  test("a save this tab cannot read back is reported, not downloaded", async () => {
    // A backend that keeps its bytes elsewhere: the reply names a key, and
    // nothing under it is here.
    const backend = new ScriptedBackend();
    backend.replies.set("session_new", { body: { result: "session", session: 1 }, rev: 1 });
    backend.replies.set("save", { body: { result: "saved", path: "out/akari.clm" }, rev: 1 });
    const editor = await Editor.create(fakeWasm().module, backend);
    const session = await editor.newDocument();
    const download = stubDownload();
    let api: FileSaver | undefined;

    function Host() {
      api = useFileSave(session);
      return null;
    }
    const view = await mount(
      <EditorProvider editor={editor}>
        <Host />
      </EditorProvider>,
    );

    expect(await api?.save("akari")).toEqual({ key: "out/akari.clm", downloaded: false });
    expect(download.names).toEqual([]);

    await view.unmount();
    download.restore();
  });

  test("the key is the name flattened, sanitized, and ending in .clm", () => {
    expect(saveKey("copy")).toBe("copy.clm");
    expect(saveKey("models/Akari Final.CLM")).toBe("Akari_Final.CLM");
    expect(saveKey("rig.inx")).toBe("rig.inx.clm");
    expect(saveKey("")).toBe("untitled.clm");
  });
});

/**
 * Catches what a download hands the browser: the blob behind the object URL,
 * and the name on the anchor that was clicked. happy-dom would otherwise
 * navigate the window to the blob URL.
 */
function stubDownload(): { blobs: Blob[]; names: string[]; restore(): void } {
  const blobs: Blob[] = [];
  const names: string[] = [];
  const saved = {
    create: URL.createObjectURL,
    revoke: URL.revokeObjectURL,
    click: HTMLAnchorElement.prototype.click,
  };
  URL.createObjectURL = (blob: Blob | MediaSource): string => {
    blobs.push(blob as Blob);
    return `blob:test/${blobs.length}`;
  };
  URL.revokeObjectURL = (): void => {};
  HTMLAnchorElement.prototype.click = function click(this: HTMLAnchorElement): void {
    if (this.hasAttribute("data-catchlight-download")) names.push(this.download);
  };
  return {
    blobs,
    names,
    restore: () => {
      URL.createObjectURL = saved.create;
      URL.revokeObjectURL = saved.revoke;
      HTMLAnchorElement.prototype.click = saved.click;
    },
  };
}
