import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { Editor, Session } from "@catchlight/core";
import type { FakeEditor } from "@catchlight/core/fakes";

import {
  EditorProvider,
  NodeTree,
  ReviewPanel,
  ReviewProvider,
  SelectionProvider,
  useSelection,
} from "./index.js";
import type { Selection } from "./index.js";
import { harness, mount, run, settle } from "./test/harness.js";
import type { Mounted } from "./test/harness.js";

async function review(): Promise<{
  editor: Editor;
  wasm: FakeEditor;
  session: Session;
  view: Mounted;
  selection: () => Selection;
  attention: () => number;
  problems: () => unknown[];
}> {
  const { editor, wasm } = await harness();
  const session = await editor.newSession();
  await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }));
  let selection: Selection | undefined;
  let attention = 0;
  const problems: unknown[] = [];
  function Watch() {
    selection = useSelection();
    return null;
  }
  const view = await mount(
    <EditorProvider editor={editor}>
      <SelectionProvider session={session}>
        <ReviewProvider
          session={session}
          onError={(cause) => {
            problems.push(cause);
          }}
          onAttention={() => {
            attention += 1;
          }}
        >
          <NodeTree.Root session={session} />
          <ReviewPanel session={session} connect={<p data-connect="">bring an agent</p>} />
          <Watch />
        </ReviewProvider>
      </SelectionProvider>
    </EditorProvider>,
  );
  await settle();
  return {
    editor,
    wasm,
    session,
    view,
    selection: () => selection!,
    attention: () => attention,
    problems: () => problems,
  };
}

function button(view: Mounted, selector: string): HTMLButtonElement {
  const found = view.container.querySelector<HTMLButtonElement>(selector);
  if (!found) throw new Error(`no ${selector}`);
  return found;
}

async function write(view: Mounted, selector: string, text: string): Promise<void> {
  const area = view.container.querySelector<HTMLTextAreaElement>(selector);
  if (!area) throw new Error(`no ${selector}`);
  await run(() => {
    const set = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
    if (!set) throw new Error("HTMLTextAreaElement.value is not an accessor here");
    set.call(area, text);
    area.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function comment(view: Mounted, text: string): Promise<void> {
  await run(() => button(view, "[data-catchlight-review-new]").click());
  await write(view, "[data-catchlight-review-composer] textarea", text);
  await run(() => button(view, "[data-catchlight-review-composer] button[type=submit]").click());
  await settle();
}

describe("the review panel", () => {
  test("a comment lands on the selection, unsent, and is badged in the tree", async () => {
    const { view, wasm, selection } = await review();
    await run(() => selection().select("root/part-1"));

    await comment(view, "the fringe clips the brow");

    const thread = wasm.reviews.get(1)?.threads[0];
    expect(thread?.target).toEqual({ kind: "node", node: "root/part-1" });
    expect(thread?.comments[0]).toMatchObject({ body: "the fringe clips the brow", delivery: "unsent" });
    expect(
      view.container.querySelector("[data-catchlight-review-comment]")?.getAttribute("data-delivery"),
    ).toBe("unsent");
    expect(
      view.container.querySelector('[data-node="root/part-1"] [data-catchlight-review-badge]')?.textContent,
    ).toBe("1");
    await view.unmount();
  });

  test("nothing selected comments on the whole model", async () => {
    const { view, wasm } = await review();
    await comment(view, "feels stiff overall");
    expect(wasm.reviews.get(1)?.threads[0]?.target).toEqual({ kind: "model" });
    await view.unmount();
  });

  test("send hands every unsent comment over, then has nothing left to send", async () => {
    const { view, wasm } = await review();
    const send = () => button(view, "[data-catchlight-review-header] [data-catchlight-review-send]");
    expect(send().disabled).toBe(true);

    await comment(view, "one");
    await comment(view, "two");
    expect(send().textContent).toContain("2");

    await run(() => send().click());
    await settle();

    expect(wasm.reviews.get(1)?.threads.map((t) => t.comments[0]?.delivery)).toEqual(["sent", "sent"]);
    expect(send().disabled).toBe(true);
    await view.unmount();
  });

  test("an agent taking and answering a thread shows as working, then as a reply to read", async () => {
    const { view, session } = await review();
    await comment(view, "raise the collar");
    await run(() => session.sendReview({ cmd: "review_submit" }));

    await run(() => session.sendReview({ cmd: "review_fetch", agent: "claude" }));
    await settle();
    const card = () => view.container.querySelector("[data-catchlight-review-thread]");
    expect(card()?.hasAttribute("data-working")).toBe(true);
    expect(view.container.querySelector("[data-catchlight-review]")?.getAttribute("data-phase")).toBe(
      "working",
    );

    await run(() =>
      session.sendReview({
        cmd: "review_comment_add",
        thread: 1,
        author: { role: "agent", name: "claude" },
        body: "raised it 12px",
        images: [],
      }),
    );
    await settle();
    expect(card()?.hasAttribute("data-working")).toBe(false);
    expect(card()?.hasAttribute("data-attention")).toBe(true);
    expect(card()?.querySelector('[data-role="agent"] p')?.textContent).toBe("raised it 12px");
    await view.unmount();
  });

  test("the host's way to bring an agent in shows until one is attending", async () => {
    const { view, session } = await review();
    expect(view.container.querySelector("[data-connect]")).not.toBeNull();

    await comment(view, "raise the collar");
    await run(() => session.sendReview({ cmd: "review_submit" }));
    await run(() => session.sendReview({ cmd: "review_fetch", agent: "claude" }));
    await settle();

    expect(view.container.querySelector("[data-connect]")).toBeNull();
    await view.unmount();
  });

  test("a pasted image goes up first and rides on the comment, which may then have no text", async () => {
    const { view, wasm } = await review();
    await run(() => button(view, "[data-catchlight-review-new]").click());
    const area = view.container.querySelector("[data-catchlight-review-composer] textarea")!;
    const submit = () => button(view, "[data-catchlight-review-composer] button[type=submit]");
    expect(submit().disabled).toBe(true);

    const paste = new Event("paste", { bubbles: true, cancelable: true });
    Object.assign(paste, {
      clipboardData: { files: [new File([new Uint8Array([1, 2, 3])], "shot.png", { type: "image/png" })] },
    });
    await run(() => area.dispatchEvent(paste));
    expect(view.container.querySelectorAll("[data-catchlight-review-attachments] img")).toHaveLength(1);
    expect(submit().disabled).toBe(false);

    await run(() => submit().click());
    await settle();
    await settle();

    const comment = wasm.reviews.get(1)?.threads[0]?.comments[0];
    expect(comment?.body).toBe("");
    expect(comment?.images).toHaveLength(1);
    expect([...(wasm.reviewImages.get(comment!.images![0]!.image) ?? [])]).toEqual([1, 2, 3]);
    expect(view.container.querySelector("[data-catchlight-review-image]")).not.toBeNull();
    await view.unmount();
  });

  test("an image the editor cannot store is refused before it is attached", async () => {
    const { view, problems } = await review();
    await run(() => button(view, "[data-catchlight-review-new]").click());
    const area = view.container.querySelector("[data-catchlight-review-composer] textarea")!;
    const paste = new Event("paste", { bubbles: true, cancelable: true });
    Object.assign(paste, {
      clipboardData: { files: [new File([new Uint8Array([1])], "anim.gif", { type: "image/gif" })] },
    });
    await run(() => area.dispatchEvent(paste));

    expect(view.container.querySelector("[data-catchlight-review-attachments]")).toBeNull();
    expect(problems()).toHaveLength(1);
    await view.unmount();
  });

  test("resolving moves a thread to the resolved tab", async () => {
    const { view } = await review();
    await comment(view, "done already");

    await run(() => button(view, "[data-catchlight-review-resolve]").click());
    await settle();

    expect(view.container.querySelector("[data-catchlight-review-thread]")).toBeNull();
    const tabs = view.container.querySelectorAll<HTMLButtonElement>("[data-catchlight-review-tabs] button");
    await run(() => tabs[1]?.click());
    expect(
      view.container.querySelector("[data-catchlight-review-thread]")?.getAttribute("data-status"),
    ).toBe("resolved");
    await view.unmount();
  });

  test("a thread's target selects what it is about; a tree badge focuses the thread", async () => {
    const { view, selection, attention } = await review();
    await run(() => selection().select("root/part-1"));
    await comment(view, "here");
    await run(() => selection().select(undefined));

    await run(() => button(view, "[data-catchlight-review-target]").click());
    expect(selection().node).toBe("root/part-1");

    const before = attention();
    await run(() => button(view, "[data-catchlight-review-badge]").click());
    expect(attention()).toBe(before + 1);
    expect(
      view.container.querySelector("[data-catchlight-review-thread]")?.hasAttribute("data-focused"),
    ).toBe(true);
    await view.unmount();
  });
});
