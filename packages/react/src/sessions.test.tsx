/**
 * The two parts that talk to the editor rather than to a document: what is
 * open, and opening a file the page holds.
 */

import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { Session, SessionInfo } from "@catchlight/core";
import { act } from "react";

import { EditorProvider, FileOpen, SessionList, useSessions } from "./index.js";
import { fire, harness, mount, run, settle } from "./test/harness.js";

describe("the open documents", () => {
  test("the list follows what the editor has open, whoever opened it", async () => {
    const { editor } = await harness();

    const view = await mount(
      <EditorProvider editor={editor}>
        <SessionList.Root />
      </EditorProvider>,
    );
    await settle();
    expect(view.container.querySelectorAll("[data-catchlight-session]")).toHaveLength(0);

    // Not through this component, and not through any of its props: the editor
    // said the set changed, which is the only thing it is watching.
    await run(() => editor.newDocument("akari"));
    await settle();

    const items = [...view.container.querySelectorAll("[data-catchlight-session]")];
    expect(items).toHaveLength(1);
    expect(items[0]?.textContent).toBe("akari");
    await view.unmount();
  });

  test("picking one hands the host the whole record", async () => {
    const { editor } = await harness();
    await editor.newDocument("akari");
    const picked: SessionInfo[] = [];

    const view = await mount(
      <EditorProvider editor={editor}>
        <SessionList.Root onSelect={(info) => picked.push(info)} />
      </EditorProvider>,
    );
    await settle();

    const button = view.container.querySelector("button") as HTMLButtonElement;
    await act(async () => {
      button.click();
    });

    expect(picked).toHaveLength(1);
    expect(picked[0]).toMatchObject({ title: "akari" });
    // The id is on the element too, for a host that styles the current one.
    expect(view.container.querySelector("[data-catchlight-session]")?.getAttribute("data-session"))
      .toBe(String(picked[0]?.session));
    await view.unmount();
  });

  test("refresh is the manual door, and it rejects", async () => {
    const { editor, wasm } = await harness();
    let sessions: ReturnType<typeof useSessions> | undefined;

    function Host() {
      sessions = useSessions();
      return null;
    }
    const view = await mount(
      <EditorProvider editor={editor}>
        <Host />
      </EditorProvider>,
    );
    await settle();

    wasm.refuse.set("session_list", { code: "bad_request", message: "no" });
    await expect(sessions?.refresh()).rejects.toThrow("no");
    await view.unmount();
  });
});

describe("opening a file", () => {
  test("the bytes are read here and the document comes back open", async () => {
    const { editor, wasm } = await harness();
    const opened: Session[] = [];

    const view = await mount(
      <EditorProvider editor={editor}>
        <FileOpen.Root onOpened={(session) => opened.push(session)} />
      </EditorProvider>,
    );
    const input = view.container.querySelector("input") as HTMLInputElement;
    expect(input.getAttribute("accept")).toBe(".clm");

    const file = new File([new Uint8Array([1, 2, 3])], "Akari Final.clm");
    Object.defineProperty(input, "files", { value: [file], configurable: true });
    await fire(input, new Event("change", { bubbles: true }));
    await settle();

    expect(opened).toHaveLength(1);
    // The name is sanitized into a storage key on the way, by the editor.
    expect(wasm.requests.map((request) => request.cmd)).toContain("session_open");
    expect(wasm.requests.find((request) => request.cmd === "session_open")).toMatchObject({
      path: "Akari_Final.clm",
    });
    // Reset, so picking the same file again fires again.
    expect(input.value).toBe("");
    await view.unmount();
  });
});
