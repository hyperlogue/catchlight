import "./test/setup.js";

import { describe, expect, test } from "bun:test";
import type { Session } from "@catchlight/core";
import { useState } from "react";

import {
  EditorProvider,
  relativeTime,
  ReviewPanel,
  ReviewProvider,
  SelectionProvider,
  useVersions,
  useVersionView,
  VersionBanner,
  VersionSelect,
} from "./index.js";
import { harness, mount, run, settle } from "./test/harness.js";

const ME = { id: "me", name: "Rin" };

function Versions({ live, problems }: { live: Session; problems: unknown[] }) {
  const versions = useVersions(live, (cause) => void problems.push(cause), ME);
  const view = useVersionView(live, versions, (cause) => void problems.push(cause));
  const [asked, setAsked] = useState(0);
  return (
    <>
      <button type="button" data-ask="" onClick={() => setAsked((n) => n + 1)} />
      <VersionSelect
        versions={versions}
        current={view.viewing?.version.version}
        onSelect={(version) => (version ? view.open(version) : view.back())}
        reviewer={ME}
        saveRequested={asked}
        now={1_700_000_000_000 + 5 * 60_000}
      />
      {view.viewing && (
        <>
          <p data-shown={view.viewing.session.id} />
          <VersionBanner
            version={view.viewing.version}
            onRestore={() => void view.restore()}
            onBack={view.back}
          />
        </>
      )}
    </>
  );
}

async function setup() {
  const { editor, wasm } = await harness();
  const live = await editor.newSession("akari");
  await run(() => live.send({ cmd: "node_add", parent: "root", kind: "group", name: "a" }));
  await run(() =>
    live.sendVersion({ cmd: "version_save", label: "baseline", author: { role: "human", id: "me", name: "Rin" } }),
  );
  await run(() => live.send({ cmd: "node_add", parent: "root", kind: "group", name: "b" }));
  await run(() =>
    live.sendVersion({
      cmd: "version_save",
      summary: "Raised the ear",
      author: { role: "agent", id: "claude:1", name: "claude" },
    }),
  );
  const problems: unknown[] = [];
  const view = await mount(
    <EditorProvider editor={editor}>
      <Versions live={live} problems={problems} />
    </EditorProvider>,
  );
  await settle();
  return { editor, wasm, live, problems, view };
}

const click = (root: HTMLElement, selector: string) =>
  run(() => root.querySelector<HTMLElement>(selector)!.click());

describe("versions", () => {
  test("the selector lists versions newest first, with who saved each and when", async () => {
    const { view, problems } = await setup();
    const trigger = view.container.querySelector<HTMLButtonElement>("[data-catchlight-version-trigger]")!;
    expect(trigger.textContent).toContain("Current");
    expect(trigger.getAttribute("aria-expanded")).toBe("false");

    await click(view.container, "[data-catchlight-version-trigger]");
    const options = [...view.container.querySelectorAll<HTMLElement>('[role="option"]')];
    expect(options.map((o) => o.dataset.catchlightVersionOption)).toEqual(["current", "2", "1"]);
    expect(options[0]!.getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(options[0]!);
    expect(options[1]!.textContent).toContain("Version 2");
    expect(options[1]!.textContent).toContain("Raised the ear");
    expect(options[1]!.querySelector('[data-catchlight-version-author][data-role="agent"]')?.textContent).toBe(
      "claude",
    );
    expect(options[2]!.textContent).toContain("baseline");
    expect(options[2]!.querySelector("[data-catchlight-version-author]")?.textContent).toBe("You");
    expect(options[2]!.querySelector("time")?.textContent).toBe("5 min ago");

    await run(() =>
      view.container
        .querySelector('[role="listbox"]')!
        .dispatchEvent(new KeyboardEvent("keydown", { key: "End", bubbles: true })),
    );
    expect(document.activeElement).toBe(options[2]!);
    await run(() =>
      view.container
        .querySelector('[role="listbox"]')!
        .dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true })),
    );
    expect(document.activeElement).toBe(options[0]!);
    await run(() =>
      options[0]!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })),
    );
    expect(view.container.querySelector('[role="listbox"]')).toBeNull();
    expect(document.activeElement).toBe(trigger);
    expect(problems).toEqual([]);
  });

  test("saving from the selector numbers the next version and says how long it lasts", async () => {
    const { view, wasm } = await setup();
    wasm.versionsKept = "unsaved";
    await click(view.container, "[data-ask]");
    const field = view.container.querySelector<HTMLInputElement>('[aria-label="Version label"]')!;
    expect(document.activeElement).toBe(field);
    expect(view.container.querySelector('[data-catchlight-version-kept="stored"]')).toBeNull();
    field.value = "  hair pass ";
    await run(() =>
      view.container.querySelector("[data-catchlight-version-save]")!.dispatchEvent(
        new Event("submit", { bubbles: true, cancelable: true }),
      ),
    );
    await settle();
    const saved = wasm.requests.filter((r) => r.cmd === "version_save").at(-1);
    expect(saved).toMatchObject({ label: "hair pass", author: { role: "human", id: "me", name: "Rin" } });
    expect(view.container.querySelector('[role="listbox"]')).toBeNull();

    await click(view.container, "[data-catchlight-version-trigger]");
    expect(view.container.querySelector('[data-catchlight-version-option="3"]')?.textContent).toContain("hair pass");
    expect(view.container.querySelector('[data-catchlight-version-kept="unsaved"]')?.textContent).toBe(
      "Save the model to keep its versions.",
    );
  });

  test("picking a version shows it read-only; back closes it and restore makes it current", async () => {
    const { view, wasm, live, editor } = await setup();
    await click(view.container, "[data-catchlight-version-trigger]");
    await click(view.container, '[data-catchlight-version-option="1"]');
    await settle();

    const shown = Number(view.container.querySelector<HTMLElement>("[data-shown]")!.dataset.shown);
    expect(shown).not.toBe(live.id);
    expect(wasm.viewing.get(shown)).toEqual({ session: live.id, version: 1 });
    expect(view.container.querySelector("[data-catchlight-version-banner]")?.textContent).toContain(
      "Viewing v1 · baseline",
    );
    expect(view.container.querySelector("[data-catchlight-version-trigger]")?.textContent).toContain("v1baseline");
    const viewed = editor.session(shown)!;
    const refused = await run(() =>
      viewed.send({ cmd: "node_add", parent: "root", kind: "group", name: null }).catch((e: { code: string }) => e.code),
    );
    expect(refused).toBe("read_only");

    await click(view.container, "[data-catchlight-version-back]");
    await settle();
    expect(view.container.querySelector("[data-catchlight-version-banner]")).toBeNull();
    expect(wasm.docs.has(shown)).toBe(false);

    await click(view.container, "[data-catchlight-version-trigger]");
    await click(view.container, '[data-catchlight-version-option="1"]');
    await settle();
    const before = live.getRevision();
    await click(view.container, "[data-catchlight-version-restore]");
    await settle();
    expect(wasm.requests.some((r) => r.cmd === "version_restore" && r.session === live.id)).toBe(true);
    expect(live.getRevision()).toBe(before + 1);
    expect(wasm.snapshot(live.id).root.children.map((c) => c.name)).toEqual(["a"]);
    expect(view.container.querySelector("[data-catchlight-version-banner]")).toBeNull();
  });

  test("a reply that points at a version carries a chip that opens it", async () => {
    const { editor } = await harness();
    const live = await editor.newSession("akari");
    await run(() =>
      live.sendVersion({ cmd: "version_save", author: { role: "agent", id: "claude:1", name: "claude" } }),
    );
    await run(() =>
      live.sendReview({
        cmd: "review_thread_add",
        target: { kind: "model" },
        author: { role: "agent", id: "claude:1", name: "claude" },
        body: "Fixed.",
        pose: [],
        images: [],
        version: 1,
      }),
    );
    const opened: number[] = [];
    const view = await mount(
      <EditorProvider editor={editor}>
        <SelectionProvider session={live}>
          <ReviewProvider session={live} onError={() => {}} onOpenVersion={(n) => opened.push(n)}>
            <ReviewPanel session={live} />
          </ReviewProvider>
        </SelectionProvider>
      </EditorProvider>,
    );
    await settle();
    const chip = view.container.querySelector<HTMLButtonElement>("button[data-catchlight-version-chip]")!;
    expect(chip.textContent).toBe("v1");
    await run(() => chip.click());
    expect(opened).toEqual([1]);
  });

  test("a time reads coarsely", () => {
    const now = 1_700_000_000_000;
    expect(relativeTime(now - 10_000, now)).toBe("just now");
    expect(relativeTime(now - 3 * 3_600_000, now)).toBe("3 h ago");
    expect(relativeTime(now - 2 * 86_400_000, now)).toBe("2 d ago");
    expect(relativeTime(0, now)).toBe("");
  });
});
