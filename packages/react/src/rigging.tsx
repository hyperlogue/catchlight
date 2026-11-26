import type {
  NodeInfo,
  MotionResponse,
  ResponseSource,
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
import { flattenTree, useCommand, type ErrorHandler } from "./authoring.js";
import { useParams, useReplica } from "./replica.js";
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
  const fittedSpine = info.fitted_spine
    ? session.nodeInfo(info.fitted_spine)?.spine
    : undefined;
  const [linkOverride, setLinks] = useState<number>();
  const links = linkOverride ?? fittedSpine?.joints.length ?? 3;
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
              <>
                <Field label="Spacing">
                  <NumberField
                    label="Mesh spacing"
                    value={spacing}
                    min={1}
                    unit="px"
                    onCommit={setSpacing}
                  />
                </Field>
                <Field label="Margin">
                  <NumberField
                    label="Mesh margin"
                    value={margin}
                    min={0}
                    max={128}
                    step={1}
                    unit="px"
                    onCommit={setMargin}
                  />
                </Field>
              </>
            ) : (
              <Field label="Grid size">
                <NumberField
                  label="Mesh columns"
                  value={cols}
                  min={2}
                  max={128}
                  step={1}
                  prefix="C"
                  onCommit={setCols}
                />
                <NumberField
                  label="Mesh rows"
                  value={rows}
                  min={2}
                  max={128}
                  step={1}
                  prefix="R"
                  onCommit={setRows}
                />
              </Field>
            )}
            <button
              type="button"
              data-catchlight-action=""
              onClick={() =>
                void run({
                  cmd: "mesh_generate",
                  node: info.id,
                  mode:
                    mode === "contour"
                      ? { mode, spacing, margin }
                      : { mode, cols, rows },
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
                })
              }
            >
              <Icon name="spine" />
              Fit spine to artwork
            </button>
          </fieldset>
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
                    prefix={axis === 0 ? "X" : "Y"}
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
    </fieldset>
  );
}

export function RiggingEmpty() {
  return (
    <EmptyState icon="spine" title="Bring the drawing to life">
      Select a part to fit a spine, or a motion response to tune its movement.
    </EmptyState>
  );
}

export function MotionResponsePanel(props: AuthoringPanelProps) {
  const response = props.info.motion_response;
  return response ? <ResponseFields {...props} response={response} /> : null;
}

function ResponseFields({
  session,
  info,
  onError,
  response,
}: AuthoringPanelProps & { response: MotionResponse }) {
  const { run, busy } = useCommand(session, onError);
  const { sources, outputCandidates } = useReplica(session, (s) => {
    const nodes = flattenTree(s.tree());
    const poseInputs = new Set<string>();
    for (const node of nodes) {
      if (node.kind !== "motion_response") continue;
      const driver = s.nodeInfo(node.id)?.motion_response;
      if (!driver) continue;
      for (const { source } of driver.inputs) {
        if (source.kind === "pose_param") poseInputs.add(source.param);
      }
    }
    const params = s.params();
    const choices: { label: string; source: ResponseSource }[] = params
      .filter((p) => !p.driven_by)
      .map((p) => ({
        label: p.name,
        source: { kind: "pose_param", param: p.id },
      }));
    for (const part of nodes.filter((node) => node.kind === "part")) {
      const body = s.query({ cmd: "slot_list", node: part.id });
      if (body.result !== "slots") continue;
      for (const slot of body.slots) {
        choices.push({
          label: part.name + " / " + slot.id,
          source: {
            kind: "part_slot",
            part: part.id,
            slot: slot.id,
            angle_deg: 0,
            reference_length: 100,
          },
        });
      }
    }
    const outputCandidates = params
      .filter((p) => p.default === 0 && !poseInputs.has(p.id))
      .map((p) => ({ param: p, owner: p.driven_by }));
    return { sources: choices, outputCandidates };
  });
  const choices =
    info.parent && session.nodeInfo(info.parent)?.motion_response
      ? [
          { label: "Parent response", source: { kind: "parent" } as const },
          ...sources,
        ]
      : sources;
  const set = (patch: Partial<MotionResponse>) =>
    void run({
      cmd: "motion_response_set",
      node: info.id,
      response: { ...response, ...patch },
    });
  const setInput = (i: number, input: MotionResponse["inputs"][number]) =>
    set({
      inputs: response.inputs.map((current, j) => (j === i ? input : current)),
    });
  const sourceKey = (s: ResponseSource) =>
    s.kind === "parent"
      ? "parent"
      : JSON.stringify(s.kind === "pose_param" ? [s.param] : [s.part, s.slot]);
  return (
    <fieldset disabled={busy}>
      <Disclosure title="Motion response">
        <Field label="Period">
          <NumberField
            label="Period"
            value={response.period_s}
            min={0.001}
            step={0.01}
            unit="s"
            onCommit={(period_s) => set({ period_s })}
          />
        </Field>
        <Field label="Damping ratio">
          <NumberField
            label="Damping ratio"
            value={response.damping_ratio}
            min={0.001}
            step={0.01}
            onCommit={(damping_ratio) => set({ damping_ratio })}
          />
        </Field>
        <Field label="Output">
          <select
            aria-label="Response output"
            value={response.output_param ?? ""}
            onChange={(e) =>
              set({ output_param: e.currentTarget.value || null })
            }
          >
            <option value="">No art output</option>
            {outputCandidates
              .filter(({ owner }) => !owner || owner === info.id)
              .map(({ param: p }) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
          </select>
        </Field>
        {response.inputs.map((input, i) => {
          const { source } = input;
          return (
            <div key={i}>
              <Field label="Input">
                <select
                  aria-label={`Input ${i + 1}`}
                  value={sourceKey(source)}
                  onChange={(e) => {
                    const next = choices.find(
                      (c) => sourceKey(c.source) === e.currentTarget.value,
                    );
                    if (next) setInput(i, { ...input, source: next.source });
                  }}
                >
                  {choices.map((c) => (
                    <option
                      key={sourceKey(c.source)}
                      value={sourceKey(c.source)}
                    >
                      {c.label}
                    </option>
                  ))}
                </select>
              </Field>
              <Field label="Gain">
                <NumberField
                  label={`Input ${i + 1} gain`}
                  value={input.gain}
                  step={0.01}
                  onCommit={(gain) => setInput(i, { ...input, gain })}
                />
              </Field>
              {source.kind === "part_slot" && (
                <>
                  <Field label="Angle">
                    <NumberField
                      label="Projection angle"
                      value={source.angle_deg}
                      unit="°"
                      onCommit={(angle_deg) =>
                        setInput(i, {
                          ...input,
                          source: { ...source, angle_deg },
                        })
                      }
                    />
                  </Field>
                  <Field label="Reference length">
                    <NumberField
                      label="Reference length"
                      value={source.reference_length}
                      min={0.001}
                      onCommit={(reference_length) =>
                        setInput(i, {
                          ...input,
                          source: { ...source, reference_length },
                        })
                      }
                    />
                  </Field>
                </>
              )}
              <button
                type="button"
                onClick={() =>
                  set({ inputs: response.inputs.filter((_, j) => i !== j) })
                }
              >
                Remove input
              </button>
            </div>
          );
        })}
        <select
          aria-label="Add response input"
          value=""
          onChange={(e) => {
            const next = choices.find(
              (c) => sourceKey(c.source) === e.currentTarget.value,
            );
            if (next)
              set({
                inputs: [...response.inputs, { source: next.source, gain: 1 }],
              });
          }}
        >
          <option value="">Add input…</option>
          {choices.map((c) => (
            <option key={sourceKey(c.source)} value={sourceKey(c.source)}>
              {c.label}
            </option>
          ))}
        </select>
      </Disclosure>
    </fieldset>
  );
}
