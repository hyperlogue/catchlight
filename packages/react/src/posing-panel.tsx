import { useBindings } from "./bindings.js";
/** Posing is preview state. Param definitions and binding cells are authored
 * only through their explicit editors, so a slider never creates undo work. */
import type { BindingParams, NodeId, ParamId, ParamInfo, Session } from "@catchlight/core";
import { useEffect, useState } from "react";
import { useNodeInfo, useParams } from "./replica.js";
import { useParamActions } from "./param-actions.js";
import { ParamSliderRoot, ParamKeysRoot, useParamValue } from "./param-slider.js";
import { ParamFieldsRoot } from "./param-list.js";
import { BindingGridRoot } from "./binding-grid.js";
import {
  Disclosure,
  EmptyState,
  Field,
  Icon,
  IconButton,
  Modal,
  NumberField,
  round,
} from "./controls.js";
import { useCommand, type ErrorHandler } from "./authoring.js";

export function ParamsPanel({
  session,
  selected,
  onSelect,
  onError,
}: {
  session: Session;
  selected: ParamId | undefined;
  onSelect: (id: ParamId) => void;
  onError: ErrorHandler;
}) {
  const params = useParams(session);
  const [search, setSearch] = useState("");
  const [adding, setAdding] = useState(false);
  const [settings, setSettings] = useState<ParamId | undefined>();
  const showing = params.filter((p) =>
    p.name.toLocaleLowerCase().includes(search.toLocaleLowerCase()),
  );
  const edit = params.find((p) => p.id === settings);
  return (
    <div data-catchlight-params-panel="">
      <div data-catchlight-panel-tools="">
        <label data-catchlight-search="">
          <Icon name="search" width="14" height="14" />
          <input
            aria-label="Search params"
            placeholder="Find a param…"
            value={search}
            onChange={(e) => setSearch(e.currentTarget.value)}
          />
        </label>
        <button type="button" onClick={() => setAdding(true)}>
          <Icon name="plus" />
          New param
        </button>
      </div>
      <div data-catchlight-param-cards="">
        {showing.map((param) => (
          <ParamCard
            key={param.id}
            session={session}
            param={param}
            selected={selected === param.id}
            onSelect={() => onSelect(param.id)}
            onSettings={() => setSettings(param.id)}
          />
        ))}
      </div>
      {!params.length && (
        <EmptyState icon="settings" title="A little control goes a long way">
          Create a param, then bind it to a part’s position, rotation, opacity, or mesh.
        </EmptyState>
      )}
      {!!params.length && !showing.length && (
        <p data-catchlight-empty="">No params match “{search}”.</p>
      )}
      {adding && (
        <NewParamDialog
          session={session}
          onError={onError}
          onClose={() => setAdding(false)}
          onAdded={onSelect}
        />
      )}
      {edit && (
        <Modal title="Param settings" onClose={() => setSettings(undefined)}>
          <ParamFieldsRoot session={session} param={edit} onError={onError} />
          <div data-catchlight-param-key-editor="">
            <p data-catchlight-hint="">
              Preview the range and default value. Edit key positions on each binding.
            </p>
            <ParamSliderRoot session={session} param={edit} />
            <ParamKeysRoot session={session} param={edit} onError={onError} />
          </div>
        </Modal>
      )}
    </div>
  );
}
function ParamCard({
  session,
  param,
  selected,
  onSelect,
  onSettings,
}: {
  session: Session;
  param: ParamInfo;
  selected: boolean;
  onSelect: () => void;
  onSettings: () => void;
}) {
  const value = useParamValue(session, param);
  return (
    <div
      data-catchlight-param-card=""
      data-selected={selected ? "" : undefined}
      onPointerDown={onSelect}
    >
      <div data-catchlight-param-card-head="">
        <button type="button" data-catchlight-param-name="" onClick={onSelect}>
          <Icon name="key" width="13" height="13" />
          {param.name}
        </button>
        <IconButton icon="settings" label={`Edit ${param.name}`} onClick={onSettings} />
      </div>
      <div data-catchlight-param-value-row="">
        <ParamSliderRoot session={session} param={param} />
        <NumberField
          label={`${param.name} value`}
          value={value}
          min={param.min}
          max={param.max}
          onCommit={(v) => session.setParam(param.id, v)}
        />
      </div>
      <div data-catchlight-param-range="">
        <span>{round(param.min)}</span>
        <button
          type="button"
          title="Restore default pose"
          onClick={() => session.setParam(param.id, param.default)}
        >
          {Math.abs(value - param.default) < 0.0001 ? "Default" : "Reset"}
        </button>
        <span>{round(param.max)}</span>
      </div>
    </div>
  );
}
function NewParamDialog({
  session,
  onError,
  onClose,
  onAdded,
}: {
  session: Session;
  onError: ErrorHandler;
  onClose: () => void;
  onAdded: (id: ParamId) => void;
}) {
  const actions = useParamActions(session);
  const [busy, setBusy] = useState(false);
  const [name, setName] = useState("");
  const [min, setMin] = useState(-1);
  const [max, setMax] = useState(1);
  const [value, setValue] = useState(0);
  const valid = name.trim() && min < max && value >= min && value <= max;
  return (
    <Modal title="Create a param" onClose={onClose}>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (!valid) return;
          setBusy(true);
          void actions
            .add({
              name: name.trim(),
              min,
              max,
              default: value,
            })
            .then((id) => {
              onAdded(id);
              onClose();
            }, onError)
            .finally(() => setBusy(false));
        }}
      >
        <label data-catchlight-form-label="">
          Name
          <input
            autoFocus
            required
            placeholder="e.g. Head turn"
            value={name}
            onChange={(e) => setName(e.currentTarget.value)}
          />
        </label>
        <Field label="Range">
          <NumberField label="Minimum" value={min} unit="Min" onCommit={setMin} />
          <NumberField label="Maximum" value={max} unit="Max" onCommit={setMax} />
        </Field>
        <Field label="Default">
          <NumberField
            label="Default value"
            value={value}
            min={min}
            max={max}
            onCommit={setValue}
          />
        </Field>
        <p data-catchlight-hint="">
          Each binding owns its key positions. Recording adds keys where you edit.
        </p>
        {min >= max || value < min || value > max ? (
          <p role="alert" data-catchlight-hint="">
            {min >= max
              ? "Maximum must be greater than minimum."
              : "Default must be inside the param’s range."}
          </p>
        ) : null}
        <div data-catchlight-dialog-actions="">
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" data-primary="" disabled={!valid || busy}>
            {busy ? "Creating…" : "Create param"}
          </button>
        </div>
      </form>
    </Modal>
  );
}

export function BindingsPanel({
  session,
  node,
  param,
  onError,
}: {
  session: Session;
  node: NodeId | undefined;
  param: ParamId | undefined;
  onError: ErrorHandler;
}) {
  const params = useParams(session);
  const [chosen, setChosen] = useState<ParamId | undefined>();
  const info = useNodeInfo(session, node);
  useEffect(() => setChosen(param), [param]);
  const showing = params.some((p) => p.id === chosen) ? chosen : (param ?? params[0]?.id);
  if (!node)
    return (
      <EmptyState icon="link" title="Connect a param to the artwork">
        Select a node in the canvas or model tree to edit its bindings.
      </EmptyState>
    );
  if (!params.length)
    return (
      <EmptyState icon="key" title="Create your first param">
        Params give your model controls you can pose, animate, and drive with physics.
      </EmptyState>
    );
  return (
    <div data-catchlight-bindings-panel="">
      <Field label="Driven by">
        <select
          aria-label="Param"
          value={showing ?? ""}
          onChange={(e) => setChosen(e.currentTarget.value)}
        >
          {params.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
        <small>{info?.name}</small>
      </Field>
      <BindingGridRoot
        key={`${node}:${showing}`}
        session={session}
        node={node}
        param={showing}
        onError={onError}
      />
      {showing && info?.vertex_count ? (
        <Disclosure title="Author a mesh deform" defaultOpen={false}>
          <p data-catchlight-hint="">
            Apply an offset, rotation, or scale to the rest mesh at a key. This replaces the deform
            at that key.
          </p>
          <DeformPanel
            key={`${node}:${showing}`}
            session={session}
            node={node}
            params={{ param: showing }}
            onError={onError}
          />
        </Disclosure>
      ) : null}
    </div>
  );
}

/** A deliberate preview sweep, never an authored animation. Stopping restores
 * the value from before playback; switching params or models stops it too. */
export function usePoseSweep(session: Session | undefined, param: ParamId | undefined) {
  const [playing, setPlaying] = useState(false);
  useEffect(() => {
    if (!playing || !session || !param) return;
    const p = session.params().find((p) => p.id === param);
    if (!p) return;
    const before = session.paramValue(param) ?? p.default;
    let frame = 0;
    const start = performance.now();
    const tick = (now: number) => {
      const value = p.min + (Math.sin((now - start) / 1300) + 1) * 0.5 * (p.max - p.min);
      session.setParam(param, value);
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => {
      cancelAnimationFrame(frame);
      if (!session.closed) session.setParam(param, before);
    };
  }, [session, param, playing]);
  useEffect(() => setPlaying(false), [session, param]);
  return { playing, setPlaying };
}

export function DeformPanel({
  session,
  node,
  params,
  onError,
}: {
  session: Session;
  node: NodeId;
  params: BindingParams;
  onError: ErrorHandler;
}) {
  const { run, busy } = useCommand(session, onError);
  const all = useParams(session);
  const p = all.find((p) => p.id === params.param);
  const binding = useBindings(session, node).find((b) => b.target === "deform" && b.param === params.param && b.param_y == params.param_y);
  const positions = binding?.key_positions[0] ?? [0, 1];
  const [cell, setCell] = useState(0);
  const [x, setX] = useState(0);
  const [y, setY] = useState(0);
  const [rotate, setRotate] = useState(0);
  const [scale, setScale] = useState(1);
  if (!p) return null;
  return (
    <fieldset disabled={busy}>
      <Field label="Key position">
        <select
          aria-label="Deform key"
          value={cell}
          onChange={(e) => {
            const i = Number(e.currentTarget.value);
            setCell(i);
            session.setParam(p.id, p.min + (positions[i] ?? 0) * (p.max - p.min));
          }}
        >
          {positions.map((k, i) => (
            <option key={i} value={i}>
              {round(p.min + k * (p.max - p.min))}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Move">
        <NumberField label="Deform X" value={x} unit="X" onCommit={setX} />
        <NumberField label="Deform Y" value={y} unit="Y" onCommit={setY} />
      </Field>
      <Field label="Rotate">
        <NumberField label="Deform rotation" value={rotate} unit="°" onCommit={setRotate} />
      </Field>
      <Field label="Scale">
        <NumberField label="Deform scale" value={scale} onCommit={setScale} />
      </Field>
      <button
        type="button"
        onClick={() =>
          void (() => {
            const mesh = session.mesh(node);
            const radians = rotate * Math.PI / 180;
            const cosine = Math.cos(radians) * scale, sine = Math.sin(radians) * scale;
            const offsets: [number, number][] = mesh.verts.map(([vx, vy]) => {
              const dx = vx - mesh.origin[0], dy = vy - mesh.origin[1];
              return [dx * cosine - dy * sine - dx + x, dx * sine + dy * cosine - dy + y];
            });
            return run({ cmd: "edit_apply", if_rev: session.getRevision(), edits: [
              { op: "binding_add", node, ...params, target: "deform", key_positions: binding?.key_positions ?? (params.param_y ? [positions, [0, 1]] : [positions]) },
              { op: "binding_cells_set", node, ...params, target: "deform", cells: [{ cell: [cell, 0], value: { offsets } }] },
            ] });
          })()
        }
      >
        Set deform key
      </button>
    </fieldset>
  );
}
