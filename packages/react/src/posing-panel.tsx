import { useBindings } from "./bindings.js";
import { ReviewBadge, ReviewCommentButton } from "./review.js";
import type { BindingParams, NodeId, ParamId, ParamInfo, Session } from "@catchlight/core";
import { useEffect, useId, useMemo, useState } from "react";
import type { ComponentProps } from "react";
import { useNodeInfo, useParams, useTree } from "./replica.js";
import { flattenTree } from "./authoring.js";
import { useParamActions } from "./param-actions.js";
import {
  ParamSliderRoot,
  ParamKeysRoot,
  useHeldByMotion,
  useMotionPaused,
  useMotionRunning,
  useParamValue,
} from "./param-slider.js";
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
  SearchField,
  round,
} from "./controls.js";
import { useCommand, type ErrorHandler } from "./authoring.js";

export function ParamsPanel({
  session,
  selected,
  onSelect,
  onSelectNode,
  onError,
}: {
  session: Session;
  selected: ParamId | undefined;
  onSelect: (id: ParamId) => void;
  /** Selects the motion response that drives a param. */
  onSelectNode: (id: NodeId) => void;
  onError: ErrorHandler;
}) {
  const params = useParams(session);
  const tree = useTree(session);
  const names = useMemo(() => new Map(flattenTree(tree).map((n) => [n.id, n.name])), [tree]);
  const running = useMotionRunning(session);
  const drivenHeading = useId();
  const [search, setSearch] = useState("");
  const [adding, setAdding] = useState(false);
  const [settings, setSettings] = useState<ParamId | undefined>();
  const showing = params.filter((p) =>
    p.name.toLocaleLowerCase().includes(search.trim().toLocaleLowerCase()),
  );
  const posed = showing.filter((p) => !p.driven_by);
  const driven = showing.filter((p) => p.driven_by);
  const card = (param: ParamInfo) => (
    <ParamCard
      key={param.id}
      session={session}
      param={param}
      driver={param.driven_by ? (names.get(param.driven_by) ?? param.driven_by) : undefined}
      selected={selected === param.id}
      onSelect={() => onSelect(param.id)}
      onSelectDriver={() => param.driven_by && onSelectNode(param.driven_by)}
      onSettings={() => setSettings(param.id)}
    />
  );
  const edit = params.find((p) => p.id === settings);
  return (
    <div data-catchlight-params-panel="">
      <div data-catchlight-panel-tools="">
        <SearchField label="Search params" placeholder="Find a param…" value={search} onValueChange={setSearch} />
        <button type="button" onClick={() => setAdding(true)}>
          <Icon name="plus" />
          New param
        </button>
      </div>
      {!!posed.length && <div data-catchlight-param-cards="">{posed.map(card)}</div>}
      {!!driven.length && (
        <section data-catchlight-driven-params="" aria-labelledby={drivenHeading}>
          <div data-catchlight-driven-params-head="">
            <div>
              <h3 id={drivenHeading}>Driven by motion</h3>
              <p data-catchlight-hint="">
                {running
                  ? "Motion responses move these. Pause motion to pose them by hand."
                  : "Motion is paused, so these pose like any param."}
              </p>
            </div>
            <MotionToggle session={session} />
          </div>
          <div data-catchlight-param-cards="">{driven.map(card)}</div>
        </section>
      )}
      {!params.length && (
        <EmptyState icon="settings" title="A little control goes a long way">
          Create a param, then bind it to a part’s position, rotation, opacity, or mesh.
        </EmptyState>
      )}
      {!!params.length && !showing.length && (
        <EmptyState icon="search" title="No matching params">
          Try a different name or clear the search to see all params.
        </EmptyState>
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
/**
 * Pauses this tab's motion responses, never an edit. An editing mode pauses
 * them on its own; `editing` keeps the button from offering a resume then.
 */
export function MotionToggle({
  session,
  editing = false,
  ...rest
}: { session: Session; editing?: boolean } & Omit<ComponentProps<"button">, "onClick" | "children">) {
  const paused = useMotionPaused(session);
  const running = useMotionRunning(session);
  const held = editing || (!paused && !running);
  const label = held ? "Motion paused" : paused ? "Resume motion" : "Pause motion";
  return (
    <button
      type="button"
      data-catchlight-motion-toggle=""
      aria-label={label}
      aria-pressed={paused || held}
      disabled={held}
      title={
        held
          ? "Motion responses are paused while you edit"
          : paused
            ? "Let motion responses move their params again"
            : "Stop motion responses so you can pose their params by hand"
      }
      onClick={() => session.setMotionPaused(!paused)}
      {...rest}
    >
      <Icon name="response" width="13" height="13" />
      <span>{label}</span>
    </button>
  );
}

function ParamCard({
  session,
  param,
  driver,
  selected,
  onSelect,
  onSelectDriver,
  onSettings,
}: {
  session: Session;
  param: ParamInfo;
  /** The driving response's name, for a param one drives. */
  driver: string | undefined;
  selected: boolean;
  onSelect: () => void;
  onSelectDriver: () => void;
  onSettings: () => void;
}) {
  const value = useParamValue(session, param);
  const held = useHeldByMotion(session, param);
  return (
    <div
      data-catchlight-param-card=""
      data-selected={selected ? "" : undefined}
      data-driven={driver !== undefined ? "" : undefined}
      data-held={held ? "" : undefined}
      onPointerDown={onSelect}
    >
      <div data-catchlight-param-card-head="">
        <button type="button" data-catchlight-param-name="" onClick={onSelect}>
          <Icon name="key" width="13" height="13" />
          {param.name}
        </button>
        <ReviewBadge target={{ kind: "param", param: param.id }} label={param.name} />
        <ReviewCommentButton target={{ kind: "param", param: param.id }} label={param.name} />
        <IconButton icon="settings" label={`Edit ${param.name}`} onClick={onSettings} />
      </div>
      {driver !== undefined && (
        <div data-catchlight-param-driver="">
          <button
            type="button"
            title={`Select the motion response “${driver}”`}
            onClick={onSelectDriver}
          >
            <Icon name="response" width="12" height="12" />
            <span>
              Driven by <strong>{driver}</strong>
            </span>
          </button>
          {!held && <small>Motion paused</small>}
        </div>
      )}
      <div data-catchlight-param-value-row="">
        <ParamSliderRoot session={session} param={param} />
        <NumberField
          label={`${param.name} value`}
          value={value}
          min={param.min}
          max={param.max}
          disabled={held}
          onCommit={(v) => session.setParam(param.id, v)}
        />
      </div>
      <div data-catchlight-param-range="">
        <span>{round(param.min)}</span>
        <button
          type="button"
          title="Restore default pose"
          disabled={held}
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
        Params give your model controls you can pose, animate, and drive with motion responses.
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

/**
 * A sweep through the range, never authored. Stopping restores the
 * pre-playback value. A param a running motion response holds is never swept.
 */
export function usePoseSweep(session: Session | undefined, param: ParamId | undefined) {
  const [playing, setPlaying] = useState(false);
  const running = useMotionRunning(session);
  const held = running && !!session?.params().find((p) => p.id === param)?.driven_by;
  useEffect(() => {
    if (!playing || !session || !param || held) return;
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
  }, [session, param, playing, held]);
  useEffect(() => setPlaying(false), [session, param, held]);
  return { playing, setPlaying, held };
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
  const positions = binding?.key_positions[0] ?? p?.initial_positions ?? [];
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
        <NumberField label="Deform X" value={x} prefix="X" onCommit={setX} />
        <NumberField label="Deform Y" value={y} prefix="Y" onCommit={setY} />
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
            return run({
              cmd: "binding_cells_set", node, ...params, target: "deform",
              if_rev: session.getRevision(), cells: [{ cell: [cell, 0], value: { offsets } }],
            });
          })()
        }
      >
        Set deform key
      </button>
    </fieldset>
  );
}
