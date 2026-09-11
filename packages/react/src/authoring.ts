/** Authoring gestures end at this seam: commands, attachments, busy state and
 * human-visible failures. Queries subscribe to the replica's revision, with
 * their arguments in the key so a changed selection reads immediately. */
import { COMMAND_BYTES } from "@catchlight/core";
import type {
  NodeId,
  NodeKind,
  ResponseBody,
  Session,
  SessionEditCommand,
  SessionReplicaQueryCommand,
  StatusInfo,
  TreeNode,
} from "@catchlight/core";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useRevision } from "./replica.js";
import type { IconName } from "./controls.js";

export type ErrorHandler = (cause: unknown) => void;

/** History is editor-owned, so it is read from the backend. Responses are
 * ordered by request and cannot overwrite a newer session's status. */
export function useSessionStatus(session: Session | undefined) {
  const [status, setStatus] = useState<StatusInfo>();
  useEffect(() => {
    setStatus(undefined);
    if (!session) return;
    let request = 0,
      live = true;
    const refresh = () => {
      const current = ++request;
      void session
        .queryServer({ cmd: "status" })
        .then((body) => {
          if (live && current === request && body.result === "status") setStatus(body.status);
        })
        .catch(() => {
          if (live && current === request) setStatus(undefined);
        });
    };
    const unsubscribe = session.subscribe(refresh);
    refresh();
    return () => {
      live = false;
      unsubscribe();
    };
  }, [session]);
  return status;
}
export function useCommand(session: Session, onError: ErrorHandler) {
  const [pending, setPending] = useState(0);
  const live = useRef({ session, onError });
  live.current = { session, onError };
  const run = useCallback(
    async (
      command: SessionEditCommand,
      attachments?: readonly (readonly [string, Uint8Array])[],
    ): Promise<ResponseBody | undefined> => {
      setPending((n) => n + 1);
      try {
        return attachments || COMMAND_BYTES[command.cmd]
          ? await live.current.session.sendWith(command, attachments ?? [])
          : await live.current.session.send(command);
      } catch (cause) {
        live.current.onError(cause);
        return undefined;
      } finally {
        setPending((n) => n - 1);
      }
    },
    [],
  );
  return { run, busy: pending > 0 };
}
export function useModelQuery<R extends ResponseBody["result"]>(
  session: Session,
  command: SessionReplicaQueryCommand,
  result: R,
): Extract<ResponseBody, { result: R }> {
  const rev = useRevision(session);
  const key = JSON.stringify(command);
  return useMemo(() => {
    const body = session.query(JSON.parse(key) as SessionReplicaQueryCommand);
    if (body.result !== result) throw new Error(`Could not read ${result}.`);
    return body as Extract<ResponseBody, { result: R }>;
  }, [session, rev, key, result]);
}
export function flattenTree(root: TreeNode): TreeNode[] {
  return [root, ...root.children.flatMap(flattenTree)];
}
export const kindLabels: Record<NodeKind, string> = {
  group: "Group",
  part: "Part",
  composite: "Composite",
  mesh_group: "Mesh group",
  physics: "Simple physics",
  spine: "Spine",
};
export const kindIcons: Record<NodeKind, IconName> = {
  group: "group",
  part: "part",
  composite: "composite",
  mesh_group: "mesh",
  physics: "physics",
  spine: "spine",
};

/** Import bytes through the same editor commands on both backends. The part
 * is removed if its image cannot be read, so a failed import leaves no broken
 * node. Multiple images are deliberately ordered, matching their tree order. */
export async function importArtwork(
  session: Session,
  files: readonly File[],
  parent: NodeId,
): Promise<NodeId | undefined> {
  let last: NodeId | undefined;
  for (const file of files) {
    if (!/\.(png|tga)$/i.test(file.name))
      throw new Error("Choose PNG or TGA artwork. Open a .clm file to load a complete model.");
    const created = await session.send({
      cmd: "node_add",
      parent,
      kind: "part",
      name: file.name.replace(/\.[^.]+$/, ""),
    });
    if (created.result !== "node") throw new Error("The image part could not be created.");
    try {
      await session.sendWith(
        {
          cmd: "texture_add",
          node: created.node,
          encoding: /\.tga$/i.test(file.name) ? "tga" : "png",
        },
        [["texture", new Uint8Array(await file.arrayBuffer())]],
      );
      await session.send({
        cmd: "mesh_auto",
        node: created.node,
        mode: { mode: "grid", cols: 2, rows: 2 },
      });
      last = created.node;
    } catch (cause) {
      await session.send({ cmd: "node_delete", node: created.node });
      throw cause;
    }
  }
  return last;
}
