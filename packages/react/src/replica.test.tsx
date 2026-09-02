/**
 * The rule the whole package rests on: a panel reads the replica, and the
 * revision is the only thing it subscribes to.
 */

import "./test/setup.js";

import { describe, expect, test } from "bun:test";

import { EditorProvider, useNodeInfo, useParams, useRevision, useTree } from "./index.js";
import { harness, mount, run } from "./test/harness.js";

describe("reading the replica", () => {
  test("a document command moves the revision the panel is subscribed to", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();
    let renders = 0;

    function Rev() {
      renders += 1;
      return <span>{useRevision(session)}</span>;
    }

    const view = await mount(
      <EditorProvider editor={editor}>
        <Rev />
      </EditorProvider>,
    );
    expect(view.container.textContent).toBe("1");
    const before = renders;

    await run(() => session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }));

    expect(view.container.textContent).toBe("2");
    expect(renders).toBeGreaterThan(before);
    await view.unmount();
  });

  test("a read is redone when the document moves, and not otherwise", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();
    let reads = 0;

    function Panel() {
      const params = useParams(session);
      const tree = useTree(session);
      reads = params.length;
      return <span>{`${tree.children.length}/${reads}`}</span>;
    }

    const view = await mount(
      <EditorProvider editor={editor}>
        <Panel />
      </EditorProvider>,
    );
    expect(view.container.textContent).toBe("0/0");

    await run(() =>
      session.send({
        cmd: "param_add",
        name: "eye_open",
        min: 0,
        max: 1,
        default: 1,
        key_positions: [0, 1],
      }),
    );

    // Nothing told this component anything: the revision moved, so the read
    // ran again and the new param is simply there.
    expect(view.container.textContent).toBe("0/1");
    await view.unmount();
  });

  test("a node's fields are re-read when the document moves, and when the node changes", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();
    const first = await run(() =>
      session.send({ cmd: "node_add", parent: "root", kind: "part", name: "hair" }),
    );
    const second = await run(() =>
      session.send({ cmd: "node_add", parent: "root", kind: "group", name: "torso" }),
    );
    const hair = first.result === "node" ? first.node : "";
    const torso = second.result === "node" ? second.node : "";

    function Inspector({ node }: { node: string | undefined }) {
      const info = useNodeInfo(session, node);
      return <span>{info ? `${info.name}/${info.kind}/${String(info.parent)}` : "gone"}</span>;
    }

    const view = await mount(
      <EditorProvider editor={editor}>
        <Inspector node={hair} />
      </EditorProvider>,
    );
    expect(view.container.textContent).toBe("hair/part/root");

    // The revision has not moved: a hook that only watched it would still be
    // showing the node the panel just navigated away from.
    await view.render(
      <EditorProvider editor={editor}>
        <Inspector node={torso} />
      </EditorProvider>,
    );
    expect(view.container.textContent).toBe("torso/group/root");

    await view.render(
      <EditorProvider editor={editor}>
        <Inspector node="root/part-404" />
      </EditorProvider>,
    );
    expect(view.container.textContent).toBe("gone");

    // Nothing selected reads the same way, so a panel needs no second branch.
    await view.render(
      <EditorProvider editor={editor}>
        <Inspector node={undefined} />
      </EditorProvider>,
    );
    expect(view.container.textContent).toBe("gone");
    await view.unmount();
  });

  test("a pose is not an edit, so it moves no revision", async () => {
    const { editor } = await harness();
    const session = await editor.newDocument();
    const body = await session.send({
      cmd: "param_add",
      name: "eye_open",
      min: 0,
      max: 1,
      default: 1,
      key_positions: [0, 1],
    });
    const param = body.result === "param" ? body.param : "";

    function Rev() {
      return <span>{useRevision(session)}</span>;
    }

    const view = await mount(
      <EditorProvider editor={editor}>
        <Rev />
      </EditorProvider>,
    );
    const before = view.container.textContent;

    await run(() => session.setParam(param, 0.25));

    expect(view.container.textContent).toBe(before);
    await view.unmount();
  });
});
