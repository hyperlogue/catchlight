/** The selected node's authoring surface, composed from independently usable
 * parts. Each section is conditional on what that kind actually carries. */
import type { Session } from "@catchlight/core";
import { useEffect, useRef, useState } from "react";
import { useSelection } from "./selection.js";
import { useNodeInfo, useReplica } from "./replica.js";
import { InspectorRoot } from "./inspector.js";
import { Disclosure, EmptyState, Icon, IconButton } from "./controls.js";
import { flattenTree, kindLabels, kindIcons, useCommand, type ErrorHandler } from "./authoring.js";
import { MaskPanel, SlotsPanel } from "./model-panels.js";
import { MeshPanel, PhysicsPanel, SpinePanel } from "./rigging.js";

export function PropertiesPanel({ session, onError }: { session: Session; onError: ErrorHandler }) {
  const { node } = useSelection();
  const info = useNodeInfo(session, node);
  if (!info)
    return (
      <EmptyState title="Make a selection">
        Click artwork on the canvas or choose a node in the tree to see its properties.
      </EmptyState>
    );
  return (
    <div key={info.id} data-catchlight-properties="">
      <div data-catchlight-selection-heading="">
        <Icon name={kindIcons[info.kind]} />
        <div>
          <strong>{info.name}</strong>
          <small>{kindLabels[info.kind]}</small>
        </div>
      </div>
      <Disclosure title="Properties">
        <InspectorRoot session={session} onError={onError} />
      </Disclosure>
      {info.kind === "part" && <ArtworkUpload session={session} node={info.id} onError={onError} />}
      <MaskPanel session={session} info={info} onError={onError} />
      <MeshPanel session={session} info={info} onError={onError} />
      <SpinePanel session={session} info={info} onError={onError} />
      <PhysicsPanel session={session} info={info} onError={onError} />
      {info.kind === "part" && <SlotsPanel session={session} info={info} onError={onError} />}
      <Disclosure title="Identity" defaultOpen={false}>
        <p data-catchlight-hint="">Node ID</p>
        <code data-catchlight-id="">{info.id}</code>
        {info.parent && (
          <>
            <p data-catchlight-hint="">Parent</p>
            <code data-catchlight-id="">{info.parent}</code>
          </>
        )}
      </Disclosure>
    </div>
  );
}

function ArtworkUpload({
  session,
  node,
  onError,
}: {
  session: Session;
  node: string;
  onError: ErrorHandler;
}) {
  const { run, busy } = useCommand(session, onError);
  return (
    <label data-catchlight-upload-art="">
      <Icon name="upload" />
      Replace artwork
      <input
        type="file"
        accept=".png,.tga"
        aria-label="Replace artwork"
        disabled={busy}
        onChange={(e) => {
          const file = e.currentTarget.files?.[0];
          e.currentTarget.value = "";
          if (!file) return;
          void file.arrayBuffer().then(
            (bytes) =>
              run(
                {
                  cmd: "texture_add",
                  node,
                  encoding: /\.tga$/i.test(file.name) ? "tga" : "png",
                },
                [["texture", new Uint8Array(bytes)]],
              ),
            onError,
          );
        }}
      />
    </label>
  );
}

export function AssetsPanel({ session }: { session: Session }) {
  const textures = useReplica(session, (s) => s.textures());
  const parts = useReplica(session, (s) =>
    flattenTree(s.tree())
      .filter((n) => n.kind === "part")
      .map((n) => s.nodeInfo(n.id)!),
  );
  const { select } = useSelection();
  const [search, setSearch] = useState("");
  return (
    <>
      <label data-catchlight-search="">
        <Icon name="search" width="14" height="14" />
        <input
          aria-label="Search textures"
          placeholder="Find artwork…"
          value={search}
          onChange={(e) => setSearch(e.currentTarget.value)}
        />
      </label>
      {!textures.length ? (
        <EmptyState icon="part" title="Your artwork lives here">
          Import PNG or TGA images to start building your character.
        </EmptyState>
      ) : (
        <div data-catchlight-asset-list="">
          {textures
            .map((t) => ({
              texture: t,
              owners: parts.filter((p) => p.texture === t.id),
            }))
            .filter(({ texture, owners }) =>
              `${texture.id} ${owners.map((p) => p.name).join(" ")}`
                .toLowerCase()
                .includes(search.toLowerCase()),
            )
            .map(({ texture: t, owners }) => (
              <button
                type="button"
                key={t.id}
                data-catchlight-asset=""
                onClick={() => select(owners[0]?.id)}
                title={owners.map((p) => p.name).join(", ") || t.id}
              >
                <span data-catchlight-asset-thumb="">
                  <TextureThumbnail session={session} texture={t.id} />
                </span>
                <span>
                  <strong>{owners[0]?.name ?? t.id}</strong>
                  <small>
                    {t.width} × {t.height} px
                  </small>
                </span>
              </button>
            ))}
        </div>
      )}
    </>
  );
}

export function TextureThumbnail({ session, texture }: { session: Session; texture: string }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const pixels = session.textureThumbnail(texture);
    if (!pixels) return;
    canvas.width = pixels.width;
    canvas.height = pixels.height;
    canvas
      .getContext("2d")
      ?.putImageData(
        new ImageData(new Uint8ClampedArray(pixels.rgba), pixels.width, pixels.height),
        0,
        0,
      );
  }, [session, texture]);
  return <canvas ref={ref} aria-hidden="true" />;
}
