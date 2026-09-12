/** The authoring panels for meshes, spines and pendulums. Each reads the same
 * protocol types it writes; opening a panel never authors defaults. */
import type {
  ChainArg,
  LinkFeelArg,
  NodeInfo,
  Session,
} from "@catchlight/core";
import { useState } from "react";
import {
  Disclosure,
  EmptyState,
  Field,
  Icon,
  NumberField,
} from "./controls.js";
import { useCommand, type ErrorHandler } from "./authoring.js";
import { useParams } from "./replica.js";
import { useEditing } from "./editing.js";

export interface AuthoringPanelProps {
  session: Session;
  info: NodeInfo;
  onError: ErrorHandler;
}

export function MeshPanel({ session, info, onError }: AuthoringPanelProps) {
  const editing = useEditing();
  const { run, busy } = useCommand(session, onError);
  const [mode, setMode] = useState<"contour" | "grid">("contour");
  const [spacing, setSpacing] = useState(32);
  const [margin, setMargin] = useState(4);
  const [cols, setCols] = useState(6);
  const [rows, setRows] = useState(8);
  const [links, setLinks] = useState(3);
  const [spring, setSpring] = useState(2);
  const [simulate, setSimulate] = useState(true);
  const [warnings, setWarnings] = useState<string[]>([]);
  if (info.vertex_count == null) return null;
  return (
    <>
      <Disclosure
        title="Mesh"
        badge={
          <span data-catchlight-badge="">{info.vertex_count} vertices</span>
        }
      >
        <div data-catchlight-metrics="">
          <span>
            <b>{info.vertex_count}</b> vertices
          </span>
          <span>
            <b>{info.triangle_count}</b> triangles
          </span>
        </div>
        {editing && info.kind === "part" && (
          <button
            type="button"
            data-catchlight-action=""
            disabled={!info.texture}
            onClick={() => editing.requestMode("mesh")}
          >
            <Icon name="mesh" />
            Edit mesh on artwork…
          </button>
        )}
        {info.kind === "part" && !editing && (
          <fieldset disabled={busy || !info.texture}>
            <Field label="Generate">
              <select
                aria-label="Mesh method"
                value={mode}
                onChange={(e) => setMode(e.currentTarget.value as typeof mode)}
              >
                <option value="contour">Trace artwork</option>
                <option value="grid">Regular grid</option>
              </select>
            </Field>
            {mode === "contour" ? (
              <Field label="Spacing">
                <NumberField
                  label="Mesh spacing"
                  value={spacing}
                  min={1}
                  unit="px"
                  onCommit={setSpacing}
                />
              </Field>
            ) : (
              <Field label="Grid size">
                <NumberField
                  label="Mesh columns"
                  value={cols}
                  min={2}
                  max={128}
                  step={1}
                  unit="C"
                  onCommit={setCols}
                />
                <NumberField
                  label="Mesh rows"
                  value={rows}
                  min={2}
                  max={128}
                  step={1}
                  unit="R"
                  onCommit={setRows}
                />
              </Field>
            )}
            <Field label="Margin">
              <NumberField
                label="Mesh margin"
                value={margin}
                min={0}
                unit="px"
                onCommit={setMargin}
              />
            </Field>
            <button
              type="button"
              data-catchlight-action=""
              onClick={() =>
                void run({
                  cmd: "mesh_auto",
                  node: info.id,
                  mode:
                    mode === "contour"
                      ? { mode, spacing, margin }
                      : { mode, cols, rows, margin },
                })
              }
            >
              <Icon name="mesh" />
              {busy ? "Generating…" : "Generate mesh"}
            </button>
            <p data-catchlight-hint="">
              Refits existing deforms. Filled slots will need to be assigned
              again.
            </p>
          </fieldset>
        )}
        {info.kind === "part" && !info.texture && (
          <p data-catchlight-hint="">
            Add artwork to generate a mesh from its silhouette.
          </p>
        )}
      </Disclosure>
      {info.kind === "part" && (
        <Disclosure title="Fit a spine" defaultOpen={false}>
          <p data-catchlight-hint="">
            Give this part a flexible strand of joints. Ideal for hair, tails,
            ribbons, and loose clothing.
          </p>
          <fieldset disabled={busy || !info.vertex_count}>
            <Field label="Links">
              <NumberField
                label="Spine links"
                value={links}
                min={1}
                max={32}
                step={1}
                onCommit={setLinks}
              />
            </Field>
            <Field label="Physics">
              <label data-catchlight-toggle="">
                <input
                  type="checkbox"
                  checked={simulate}
                  onChange={(e) => setSimulate(e.currentTarget.checked)}
                />
                Simulate movement
              </label>
            </Field>
            {simulate && (
              <Field label="Stiffness">
                <NumberField
                  label="Fit stiffness"
                  value={spring}
                  min={0}
                  unit="Hz"
                  onCommit={setSpring}
                />
              </Field>
            )}
            <button
              type="button"
              data-catchlight-action=""
              onClick={() =>
                void run({
                  cmd: "spine_fit",
                  part: info.id,
                  links,
                  axis: null,
                  name: `${info.name} spine`,
                  chain: simulate
                    ? {
                        links: Array.from({ length: links }, () => ({
                          stiffness: spring,
                          damping: 0.5,
                        })),
                      }
                    : null,
                }).then((body) => {
                  if (body?.result === "spine_fit")
                    setWarnings(body.warnings ?? []);
                })
              }
            >
              <Icon name="spine" />
              Fit spine to artwork
            </button>
          </fieldset>
          {warnings.map((w) => (
            <p key={w} data-catchlight-warning="">
              {w}
            </p>
          ))}
        </Disclosure>
      )}
    </>
  );
}

export function SpinePanel({ session, info, onError }: AuthoringPanelProps) {
  const { run, busy } = useCommand(session, onError);
  const params = useParams(session);
  const spine = info.spine;
  if (!spine) return null;
  const chain = spine.chain;
  const setChain = (next: ChainArg | null) =>
    void run({
      cmd: "spine_set",
      node: info.id,
      joints: null,
      targets: null,
      chain: next,
    });
  const feel = (index: number, patch: LinkFeelArg) => {
    if (!chain) return;
    const links = spine.joints.map((_, i) => ({
      ...chain.links?.[i],
      ...(i === index ? patch : {}),
    }));
    setChain({ ...chain, links });
  };
  return (
    <fieldset disabled={busy}>
      <Disclosure
        title="Spine"
        badge={
          <span data-catchlight-badge="">{spine.joints.length} links</span>
        }
      >
        <p data-catchlight-hint="">
          Joints define the drawing at rest. Each bend reads its own param.
        </p>
        {spine.joints.map((point, i) => (
          <div key={i} data-catchlight-joint="">
            <span data-catchlight-index="">{i + 1}</span>
            <div>
              <Field label="Position">
                {point.map((value, axis) => (
                  <NumberField
                    key={axis}
                    label={`Joint ${i + 1} ${axis === 0 ? "X" : "Y"}`}
                    unit={axis === 0 ? "X" : "Y"}
                    value={value}
                    onCommit={(v) => {
                      const joints = spine.joints.map((p, j) =>
                        j === i
                          ? (p.map((n, a) => (a === axis ? v : n)) as [
                              number,
                              number,
                            ])
                          : p,
                      );
                      void run({
                        cmd: "spine_set",
                        node: info.id,
                        joints,
                        targets: null,
                      });
                    }}
                  />
                ))}
              </Field>
              <select
                aria-label={`Joint ${i + 1} param`}
                value={spine.targets[i] ?? ""}
                onChange={(e) => {
                  const targets = [...spine.targets];
                  targets[i] = e.currentTarget.value || null;
                  void run({
                    cmd: "spine_set",
                    node: info.id,
                    joints: null,
                    targets,
                  });
                }}
              >
                <option value="">No bend param</option>
                {params.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </select>
            </div>
          </div>
        ))}
      </Disclosure>
      <Disclosure title="Particle chain">
        <label data-catchlight-toggle="">
          <input
            type="checkbox"
            aria-label="Enable particle chain"
            checked={!!chain}
            onChange={(e) =>
              setChain(
                e.currentTarget.checked
                  ? {
                      links: spine.joints.map(() => ({
                        stiffness: 2,
                        damping: 0.5,
                      })),
                    }
                  : null,
              )
            }
          />
          Simulate this spine
        </label>
        {!chain ? (
          <p data-catchlight-hint="">
            Add natural follow-through while keeping every bend editable through
            params.
          </p>
        ) : (
          <>
            <Field label="Weight">
              <NumberField
                label="Physics weight"
                value={chain.weight ?? 1}
                min={0}
                max={1}
                step={0.05}
                onCommit={(weight) => setChain({ ...chain, weight })}
              />
            </Field>
            <Field label="Gravity">
              <NumberField
                label="Chain gravity"
                value={chain.gravity ?? 1}
                min={0.001}
                unit="g"
                onCommit={(gravity) => setChain({ ...chain, gravity })}
              />
            </Field>
            <label data-catchlight-toggle="">
              <input
                type="checkbox"
                checked={chain.local_only ?? false}
                onChange={(e) =>
                  setChain({ ...chain, local_only: e.currentTarget.checked })
                }
              />
              Use local motion only
            </label>
            {spine.joints.map((_, i) => {
              const link = chain.links?.[i];
              return (
                <Disclosure
                  key={i}
                  title={`Link ${i + 1}`}
                  defaultOpen={i === 0}
                >
                  <Field label="Stiffness">
                    <NumberField
                      label={`Link ${i + 1} stiffness`}
                      unit="Hz"
                      value={link?.stiffness ?? 0}
                      min={0}
                      onCommit={(stiffness) => feel(i, { stiffness })}
                    />
                  </Field>
                  <Field label="Damping">
                    <NumberField
                      label={`Link ${i + 1} damping`}
                      value={link?.damping ?? 0.5}
                      min={0}
                      max={1}
                      step={0.05}
                      onCommit={(damping) => feel(i, { damping })}
                    />
                  </Field>
                  <Field label="Gravity">
                    <NumberField
                      label={`Link ${i + 1} gravity`}
                      value={link?.gravity_scale ?? 1}
                      unit="×"
                      onCommit={(gravity_scale) => feel(i, { gravity_scale })}
                    />
                  </Field>
                  <label data-catchlight-toggle="">
                    <input
                      type="checkbox"
                      checked={link?.limit != null}
                      onChange={(e) => {
                        const links = spine.joints.map((_, j) => {
                          const next = { ...chain.links?.[j] };
                          if (i === j) {
                            // Omit to restore the core's default; null is unlimited.
                            if (e.currentTarget.checked) delete next.limit;
                            else next.limit = null;
                          }
                          return next;
                        });
                        setChain({ ...chain, links });
                      }}
                    />
                    Limit bend
                  </label>
                  {link?.limit != null && (
                    <Field label="Max bend">
                      <NumberField
                        label={`Link ${i + 1} max bend from rest`}
                        value={link.limit * 180}
                        min={1}
                        max={180}
                        unit="°"
                        onCommit={(v) => feel(i, { limit: v / 180 })}
                      />
                    </Field>
                  )}
                </Disclosure>
              );
            })}
          </>
        )}
      </Disclosure>
    </fieldset>
  );
}

export function PhysicsPanel({ session, info, onError }: AuthoringPanelProps) {
  const { run, busy } = useCommand(session, onError);
  const params = useParams(session);
  const p = info.physics;
  if (!p) return null;
  const set = (patch: Partial<typeof p>) =>
    void run({ cmd: "physics_set", node: info.id, ...p, ...patch });
  return (
    <fieldset disabled={busy}>
      <Disclosure title="Simple physics">
        <Field label="Type">
          <select
            aria-label="Physics type"
            value={p.kind}
            onChange={(e) =>
              set({ kind: e.currentTarget.value as typeof p.kind })
            }
          >
            <option value="rigid">Pendulum</option>
            <option value="spring">Spring pendulum</option>
          </select>
        </Field>
        {(
          [
            ["length", "Length", "px"],
            ["gravity", "Gravity", "g"],
            ["frequency", "Frequency", "Hz"],
            ["angle_damping", "Angle damping", ""],
            ["length_damping", "Length damping", ""],
          ] as const
        ).map(([key, label, unit]) => (
          <Field key={key} label={label}>
            <NumberField
              label={label}
              value={p[key]}
              min={0}
              {...(key.includes("damping") ? { max: 1 } : {})}
              unit={unit}
              onCommit={(v) => set({ [key]: v })}
            />
          </Field>
        ))}
        <label data-catchlight-toggle="">
          <input
            type="checkbox"
            checked={p.local_only}
            onChange={(e) => set({ local_only: e.currentTarget.checked })}
          />
          Use local motion only
        </label>
      </Disclosure>
      <Disclosure title="Param outputs">
        <Field label="Mapping">
          <select
            aria-label="Physics mapping"
            value={p.map_mode}
            onChange={(e) =>
              set({ map_mode: e.currentTarget.value as typeof p.map_mode })
            }
          >
            {["xy", "yx", "angle_length", "length_angle"].map((v) => (
              <option key={v}>{v}</option>
            ))}
          </select>
        </Field>
        {(["angle", "length"] as const).map((key) => (
          <Field key={key} label={key === "angle" ? "Angle" : "Length"}>
            <select
              aria-label={`${key} output param`}
              value={p.target_params[key] ?? ""}
              onChange={(e) =>
                set({
                  target_params: {
                    ...p.target_params,
                    [key]: e.currentTarget.value || null,
                  },
                })
              }
            >
              <option value="">None</option>
              {params.map((param) => (
                <option key={param.id} value={param.id}>
                  {param.name}
                </option>
              ))}
            </select>
          </Field>
        ))}
        <Field label="Output scale">
          {p.output_scale.map((v, i) => (
            <NumberField
              key={i}
              label={`Output scale ${i + 1}`}
              unit={i === 0 ? "X" : "Y"}
              value={v}
              onCommit={(n) =>
                set({
                  output_scale: p.output_scale.map((old, axis) =>
                    axis === i ? n : old,
                  ) as [number, number],
                })
              }
            />
          ))}
        </Field>
      </Disclosure>
    </fieldset>
  );
}

export function RiggingEmpty() {
  return (
    <EmptyState icon="spine" title="Bring the drawing to life">
      Select a part to fit a spine, or select a spine to tune its movement.
    </EmptyState>
  );
}
