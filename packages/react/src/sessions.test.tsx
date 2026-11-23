import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { Session, SessionInfo } from "@catchlight/core";
import { act } from "react";

import { EditorProvider, FileOpen, SessionList, useSessions } from "./index.js";
import { fire, harness, mount, run, settle } from "./test/harness.js";

describe("the open models", () => {
  test("the list follows what the editor has open, whoever opened it", async () => {
    const { editor } = await harness();

    const view = await mount(
      <EditorProvider editor={editor}>
        <SessionList.Root />
      </EditorProvider>,
    );
    await settle();
    expect(view.container.querySelectorAll("[data-catchlight-session]")).toHaveLength(0);

    await run(() => editor.newSession("akari"));
    await settle();

    const items = [...view.container.querySelectorAll("[data-catchlight-session]")];
    expect(items).toHaveLength(1);
    expect(items[0]?.textContent).toBe("akari");
    await view.unmount();
  });

  test("a delayed list cannot restore a model after a newer list removed it", async () => {
    const { editor } = await harness();
    const session = await editor.newSession("akari");
    const listSessions = editor.listSessions.bind(editor);
    const stale = await listSessions();
    let release!: (sessions: typeof stale) => void;
    const pending = new Promise<typeof stale>((resolve) => { release = resolve; });
    editor.listSessions = () => {
      editor.listSessions = listSessions;
      return pending;
    };
    const view = await mount(
      <EditorProvider editor={editor}><SessionList.Root /></EditorProvider>,
    );
    await run(() => editor.closeSession(session.id));
    await settle();
    await run(() => release(stale));
    await settle();
    expect(view.container.querySelectorAll("[data-catchlight-session]")).toHaveLength(0);
    await view.unmount();
    editor.close();
  });

  test("picking one hands the host the whole record", async () => {
    const { editor } = await harness();
    await editor.newSession("akari");
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
    expect(view.container.querySelector("[data-catchlight-session]")?.getAttribute("data-session"))
      .toBe(String(picked[0]?.session));
    await view.unmount();
  });

  test("a close control appears only with a handler, and the current row is marked", async () => {
    const { editor } = await harness();
    const session = await editor.newSession("akari");
    await editor.newSession("beni");
    const closed: SessionInfo[] = [];

    const view = await mount(
      <EditorProvider editor={editor}>
        <SessionList.Root current={session.id} />
      </EditorProvider>,
    );
    await settle();
    expect(view.container.querySelectorAll("[data-catchlight-session-close]")).toHaveLength(0);
    expect(
      view.container.querySelector("[data-catchlight-session][data-current]")?.getAttribute("data-session"),
    ).toBe(String(session.id));

    await view.render(
      <EditorProvider editor={editor}>
        <SessionList.Root current={session.id} onClose={(info) => closed.push(info)} />
      </EditorProvider>,
    );
    const controls = view.container.querySelectorAll<HTMLButtonElement>("[data-catchlight-session-close]");
    expect(controls).toHaveLength(2);
    expect(controls[1]?.getAttribute("aria-label")).toBe("Close beni");
    await act(async () => {
      controls[1]?.click();
    });

    expect(closed).toHaveLength(1);
    expect(closed[0]).toMatchObject({ title: "beni" });
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
  test("the bytes are read here and the model comes back open", async () => {
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
    expect(wasm.requests.map((request) => request.cmd)).toContain("session_create");
    expect(wasm.requests.find((request) => request.cmd === "session_create")).toMatchObject({
      name: "Akari_Final.clm",
    });
    expect(input.value).toBe("");
    await view.unmount();
  });
});
